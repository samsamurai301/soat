//! Pricing.
//!
//! An item (a variant, or a product without variants) has its own price in
//! its product's currency. Price lists add prices in any currency, with an
//! optional compare-at ("was") price. [`resolve`] picks the price a buyer
//! pays: the active list in the asked currency with the highest priority
//! that prices the item, else the item's own price when the currency
//! matches, else none: the item is not sold in that currency.
//!
//! Customer groups and quantity tiers come in B11; markets that convert
//! with exchange rates in D10.

use std::sync::Arc;

use soat_catalog::stock_item;
use soat_core::{id_kind, Currency, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_tenancy::{Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

id_kind!(PriceListId, PriceListKind, "pl");

/// Most prices set in one request.
pub const MAX_BATCH: usize = 500;

/// See price lists and resolved prices.
pub const PRICING_READ: Permission =
    Permission::new("pricing:read", "See price lists and resolved prices").for_staff();
/// Create price lists and set their prices.
pub const PRICING_WRITE: Permission =
    Permission::new("pricing:write", "Create price lists and set their prices").for_staff();

/// The pricing module.
pub struct Pricing;

impl Module for Pricing {
    fn name(&self) -> &'static str {
        "pricing"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[PRICING_READ, PRICING_WRITE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Cleanup)]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_price_lists,
            create_price_list,
            get_price_list,
            update_price_list,
            delete_price_list,
        ))
        .mount((list_prices, set_prices, delete_price, resolved_price))
    }
}

/// A price list.
#[model]
#[derive(sqlx::FromRow)]
pub struct PriceList {
    /// e.g. `pl_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, e.g. `Switzerland` or `Black Friday`.
    pub name: String,
    /// ISO 4217, e.g. `CHF`.
    pub currency: String,
    /// Higher wins when several active lists price an item.
    pub priority: i32,
    pub active: bool,
}

/// Body of a create request.
#[model]
pub struct NewPriceList {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    pub currency: String,
    pub priority: Option<i32>,
    pub active: Option<bool>,
}

/// Body of an update request; the currency is fixed.
#[model]
pub struct PriceListChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    pub priority: Option<i32>,
    pub active: Option<bool>,
}

/// An item's price in a list.
#[model]
#[derive(sqlx::FromRow)]
pub struct Price {
    pub item_id: String,
    pub price_minor: i64,
    /// The struck-through "was" price, above `price_minor`.
    pub compare_at_minor: Option<i64>,
}

/// Body of `PUT /api/price-lists/{id}/prices`.
#[model]
pub struct SetPrices {
    pub prices: Vec<Price>,
}

/// What a buyer pays for an item in a currency.
#[model]
pub struct ResolvedPrice {
    pub item_id: String,
    pub currency: String,
    pub price_minor: i64,
    pub compare_at_minor: Option<i64>,
    /// The price list it comes from, or null for the item's own price.
    pub price_list_id: Option<String>,
}

const COLUMNS: &str = "id, name, currency, priority, active";

/// The price of `item_id` in `currency`, or `None` when it is not sold in
/// that currency (or does not exist). Run it in the tenant transaction.
pub async fn resolve(
    conn: &mut PgConnection,
    tenant: &Tenant,
    item_id: &str,
    currency: Currency,
) -> Result<Option<ResolvedPrice>> {
    let listed: Option<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT l.id, p.price_minor, p.compare_at_minor \
         FROM price_list_prices p JOIN price_lists l ON l.id = p.price_list_id \
         WHERE p.tenant_id = $1 AND p.item_id = $2 AND l.currency = $3 AND l.active \
         ORDER BY l.priority DESC, l.id LIMIT 1",
    )
    .bind(tenant.id())
    .bind(item_id)
    .bind(currency.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((list, price_minor, compare_at_minor)) = listed {
        return Ok(Some(ResolvedPrice {
            item_id: item_id.to_owned(),
            currency: currency.as_str().to_owned(),
            price_minor,
            compare_at_minor,
            price_list_id: Some(list),
        }));
    }
    let item = stock_item(conn, tenant, item_id).await?;
    Ok(item
        .filter(|i| i.currency == currency.as_str())
        .map(|i| ResolvedPrice {
            item_id: i.id,
            currency: i.currency,
            price_minor: i.price_minor,
            compare_at_minor: None,
            price_list_id: None,
        }))
}

fn currency(code: &str) -> Result<Currency> {
    Currency::parse(code)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))
}

async fn load(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<PriceList> {
    let id = PriceListId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, PriceList>(&format!(
        "SELECT {COLUMNS} FROM price_lists WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's price lists (`pricing:read`). `?currency=CHF` filters.
#[get("/api/price-lists", tag = "pricing")]
async fn list_price_lists(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<PriceList>>> {
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let page = TenantListing::new("price_lists", COLUMNS)
        .sortable(["name", "priority"])
        .search(["name"])
        .filter_text("currency")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a price list (`pricing:write`). Publishes `price_list.created`.
#[post("/api/price-lists", tag = "pricing", status = 201)]
async fn create_price_list(
    db: Db,
    tenant: Tenant,
    Valid(Json(b)): Valid<Json<NewPriceList>>,
) -> Result<(StatusCode, Json<PriceList>)> {
    let currency = currency(&b.currency)?;
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let list = sqlx::query_as::<_, PriceList>(&format!(
        "INSERT INTO price_lists (id, tenant_id, name, currency, priority, active) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(PriceListId::new().as_str())
    .bind(tenant.id())
    .bind(&b.name)
    .bind(currency.as_str())
    .bind(b.priority.unwrap_or(0))
    .bind(b.active.unwrap_or(true))
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "price_list.created", &list).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(list)))
}

/// One price list (`pricing:read`).
#[get("/api/price-lists/{id}", tag = "pricing")]
async fn get_price_list(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<PriceList>> {
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let list = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(list))
}

/// Rename, reprioritize, or switch a list on or off (`pricing:write`).
/// Publishes `price_list.updated`.
#[patch("/api/price-lists/{id}", tag = "pricing")]
async fn update_price_list(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<PriceListChange>>,
) -> Result<Json<PriceList>> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let list = sqlx::query_as::<_, PriceList>(&format!(
        "UPDATE price_lists SET name = COALESCE($3, name), priority = COALESCE($4, priority), \
         active = COALESCE($5, active) WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&c.name)
    .bind(c.priority)
    .bind(c.active)
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "price_list.updated", &list).await?;
    tx.commit().await?;
    Ok(Json(list))
}

/// Delete a price list and its prices (`pricing:write`). Publishes
/// `price_list.deleted`.
#[delete("/api/price-lists/{id}", tag = "pricing", status = 204)]
async fn delete_price_list(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM price_lists WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "price_list.deleted", &current)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The prices in a list, 50 per page (`pricing:read`). `?item_id=` filters.
#[get("/api/price-lists/{id}/prices", tag = "pricing")]
async fn list_prices(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Price>>> {
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let list = load(&mut tx, &tenant, &id).await?;
    let page = TenantListing::new(
        "(SELECT item_id AS id, * FROM price_list_prices) AS prices",
        "item_id, price_minor, compare_at_minor",
    )
    .scope_text("price_list_id", list.id)
    .sortable(["price_minor"])
    .filter_text("item_id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Set prices in a list, up to 500 at once (`pricing:write`). Items are
/// variants or products without variants; an unknown item, or a
/// compare-at price not above the price, is a 422 and nothing changes.
/// Publishes `price_list.prices_changed`.
#[put("/api/price-lists/{id}/prices", tag = "pricing")]
async fn set_prices(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(body): Json<SetPrices>,
) -> Result<Json<Vec<Price>>> {
    if body.prices.len() > MAX_BATCH {
        return Err(
            Error::unprocessable(format!("at most {MAX_BATCH} prices at once"))
                .with_code("too_many_prices"),
        );
    }
    for p in &body.prices {
        if p.price_minor < 0 || p.compare_at_minor.is_some_and(|c| c <= p.price_minor) {
            return Err(Error::unprocessable(format!(
                "{}: prices are at least 0 and a compare-at price is above the price",
                p.item_id
            ))
            .with_code("invalid_price"));
        }
    }
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let list = load(&mut tx, &tenant, &id).await?;
    for p in &body.prices {
        let item = stock_item(&mut tx, &tenant, &p.item_id)
            .await?
            .ok_or_else(|| {
                Error::unprocessable(format!("no item {}", p.item_id)).with_code("unknown_item")
            })?;
        sqlx::query(
            "INSERT INTO price_list_prices \
             (tenant_id, price_list_id, item_id, price_minor, compare_at_minor) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (price_list_id, item_id) DO UPDATE \
             SET price_minor = EXCLUDED.price_minor, compare_at_minor = EXCLUDED.compare_at_minor",
        )
        .bind(tenant.id())
        .bind(&list.id)
        .bind(&item.id)
        .bind(p.price_minor)
        .bind(p.compare_at_minor)
        .execute(&mut *tx)
        .await?;
    }
    tenant
        .publish(
            &mut tx,
            "price_list.prices_changed",
            &serde_json::json!({ "price_list_id": list.id, "prices": body.prices }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(body.prices))
}

/// Remove an item's price from a list (`pricing:write`). Publishes
/// `price_list.prices_changed`.
#[delete(
    "/api/price-lists/{id}/prices/{item_id}",
    tag = "pricing",
    status = 204
)]
async fn delete_price(
    db: Db,
    tenant: Tenant,
    Path((id, item_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let list = load(&mut tx, &tenant, &id).await?;
    let gone = sqlx::query(
        "DELETE FROM price_list_prices WHERE tenant_id = $1 AND price_list_id = $2 AND item_id = $3",
    )
    .bind(tenant.id())
    .bind(&list.id)
    .bind(&item_id)
    .execute(&mut *tx)
    .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "price_list.prices_changed",
            &serde_json::json!({ "price_list_id": list.id, "removed": [item_id] }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a buyer pays for an item in `?currency=` (`pricing:read`): 404
/// when the item is not sold in that currency.
#[get("/api/prices/{item_id}", tag = "pricing")]
async fn resolved_price(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    params: ListParams,
) -> Result<Json<ResolvedPrice>> {
    let code = params
        .get("currency")
        .ok_or_else(|| Error::bad_request("pass ?currency=EUR").with_code("missing_currency"))?;
    let currency = currency(code)?;
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let price = resolve(&mut tx, &tenant, &item_id, currency).await?;
    tx.commit().await?;
    price
        .map(Json)
        .ok_or_else(|| Error::not_found().with_code("no_price"))
}

/// Drops the list prices of deleted products and variants. Idempotent.
struct Cleanup;

impl Subscriber for Cleanup {
    fn name(&self) -> &'static str {
        "pricing"
    }

    fn wants(&self, kind: &str) -> bool {
        matches!(kind, "product.deleted" | "variant.deleted")
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let mut items: Vec<String> = event.payload["id"]
                .as_str()
                .map(str::to_owned)
                .into_iter()
                .collect();
            if let Some(ids) = event.payload["variant_ids"].as_array() {
                items.extend(ids.iter().filter_map(|v| v.as_str().map(str::to_owned)));
            }
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM price_list_prices WHERE tenant_id = $1 AND item_id = ANY($2)")
                .bind(&event.tenant_id)
                .bind(&items)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
    }
}
