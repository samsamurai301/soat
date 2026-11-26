//! Pricing.
//!
//! An item (a variant, or a product without variants) has its own price in
//! its product's currency. Price lists add prices in any currency, with an
//! optional compare-at ("was") price. [`resolve`] picks the price a buyer
//! pays: the active list in the asked currency with the highest priority
//! that prices the item, else the item's own price when the currency
//! matches, else none: the item is not sold in that currency.
//!
//! [`markets`] group countries that buy in one currency; with an exchange
//! rate they convert items priced in their base currency when no list
//! prices them.
//!
//! A list can be for one customer group (customers with a tag), and a
//! price in it can start at a quantity: [`resolve_for`] takes the buyer's
//! tags and the quantity, and picks the largest tier that quantity reaches
//! in the best list that applies.

pub mod markets;

use std::sync::Arc;

use soat_catalog::stock_item;
use soat_core::{id_kind, Currency, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

pub use markets::{active_markets, market_for, Market};

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
        .mount((
            markets::list_markets,
            markets::create_market,
            markets::get_market,
            markets::update_market,
            markets::delete_market,
            markets::storefront_market,
        ))
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
    /// Only customers with this tag get the list's prices; null is everyone.
    pub customer_tag: Option<String>,
}

/// Body of a create request.
#[model]
pub struct NewPriceList {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    pub currency: String,
    pub priority: Option<i32>,
    pub active: Option<bool>,
    /// A customer tag, e.g. `wholesale`: the list is for that group only.
    #[validate(length(max = 100))]
    pub customer_tag: Option<String>,
}

/// Body of an update request; the currency is fixed.
#[model]
pub struct PriceListChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    pub priority: Option<i32>,
    pub active: Option<bool>,
    /// A customer tag; an empty string opens the list to everyone.
    #[validate(length(max = 100))]
    pub customer_tag: Option<String>,
}

/// An item's price in a list.
#[model]
#[derive(sqlx::FromRow)]
pub struct Price {
    pub item_id: String,
    pub price_minor: i64,
    /// The struck-through "was" price, above `price_minor`.
    pub compare_at_minor: Option<i64>,
    /// The quantity from which this price applies (a tier); 1 when left out.
    pub min_quantity: Option<i32>,
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
    /// The market whose exchange rate converted the item's own price.
    pub market_id: Option<String>,
    /// The tier the price comes from: 1 unless a quantity price applies.
    pub min_quantity: i32,
}

/// Who is buying, for [`resolve_for`]: the customer's tags (none for a
/// guest) and how many units.
#[derive(Debug, Clone, Copy)]
pub struct Buyer<'a> {
    pub tags: &'a [String],
    pub quantity: i64,
}

impl Default for Buyer<'_> {
    fn default() -> Self {
        Buyer {
            tags: &[],
            quantity: 1,
        }
    }
}

const COLUMNS: &str = "id, name, currency, priority, active, customer_tag";

/// Lists the buyer qualifies for: everyone's, and their groups' (tags
/// match whatever their case).
const LIST_APPLIES: &str = "(l.customer_tag IS NULL \
     OR EXISTS (SELECT 1 FROM unnest($4::text[]) t WHERE lower(t) = lower(l.customer_tag))) \
     AND p.min_quantity <= $5";

fn tag(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.contains(',') {
        return Err(
            Error::unprocessable("a customer tag has no comma").with_code("invalid_customer_tag")
        );
    }
    Ok(Some(raw.trim().to_owned()))
}

/// The price a guest pays for one unit of `item_id` in `currency`; see
/// [`resolve_for`].
pub async fn resolve(
    conn: &mut PgConnection,
    scope: &impl Scope,
    item_id: &str,
    currency: Currency,
) -> Result<Option<ResolvedPrice>> {
    resolve_for(conn, scope, item_id, currency, Buyer::default()).await
}

/// The unit price of `item_id` in `currency` for `buyer`: from the best
/// active price list that applies to them (highest priority, then the
/// largest tier their quantity reaches), else the item's own price in its
/// currency, else its own price converted by a market. `None` when it is
/// not sold in that currency (or does not exist). Run it in the tenant
/// transaction.
pub async fn resolve_for(
    conn: &mut PgConnection,
    scope: &impl Scope,
    item_id: &str,
    currency: Currency,
    buyer: Buyer<'_>,
) -> Result<Option<ResolvedPrice>> {
    let listed: Option<(String, i64, Option<i64>, i32)> = sqlx::query_as(&format!(
        "SELECT l.id, p.price_minor, p.compare_at_minor, p.min_quantity \
         FROM price_list_prices p JOIN price_lists l ON l.id = p.price_list_id \
         WHERE p.tenant_id = $1 AND p.item_id = $2 AND l.currency = $3 AND l.active \
         AND {LIST_APPLIES} \
         ORDER BY l.priority DESC, p.min_quantity DESC, l.id LIMIT 1"
    ))
    .bind(scope.tenant_id())
    .bind(item_id)
    .bind(currency.as_str())
    .bind(buyer.tags)
    .bind(buyer.quantity.clamp(1, i64::from(i32::MAX)) as i32)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((list, price_minor, compare_at_minor, min_quantity)) = listed {
        return Ok(Some(ResolvedPrice {
            item_id: item_id.to_owned(),
            currency: currency.as_str().to_owned(),
            price_minor,
            compare_at_minor,
            price_list_id: Some(list),
            market_id: None,
            min_quantity,
        }));
    }
    let Some(item) = stock_item(&mut *conn, scope, item_id).await? else {
        return Ok(None);
    };
    if item.currency == currency.as_str() {
        return Ok(Some(ResolvedPrice {
            item_id: item.id,
            currency: item.currency,
            price_minor: item.price_minor,
            compare_at_minor: None,
            price_list_id: None,
            market_id: None,
            min_quantity: 1,
        }));
    }
    let Ok(own) = Currency::parse(&item.currency) else {
        return Ok(None);
    };
    Ok(
        markets::converted(conn, scope, item.price_minor, own, currency)
            .await?
            .map(|(price_minor, market)| ResolvedPrice {
                item_id: item.id,
                currency: currency.as_str().to_owned(),
                price_minor,
                compare_at_minor: None,
                price_list_id: None,
                market_id: Some(market),
                min_quantity: 1,
            }),
    )
}

/// What `own_minor` of `own` shows as in `currency`, for listings: the
/// item's best list price for a guest buying one, else the amount itself in its own currency,
/// else converted by a market. `None` when it is not sold in `currency`.
pub async fn shown_price(
    conn: &mut PgConnection,
    scope: &impl Scope,
    item_id: &str,
    own_minor: i64,
    own: Currency,
    currency: Currency,
) -> Result<Option<i64>> {
    let listed: Option<(i64,)> = sqlx::query_as(
        "SELECT p.price_minor FROM price_list_prices p \
         JOIN price_lists l ON l.id = p.price_list_id \
         WHERE p.tenant_id = $1 AND p.item_id = $2 AND l.currency = $3 AND l.active \
         AND l.customer_tag IS NULL AND p.min_quantity = 1 \
         ORDER BY l.priority DESC, l.id LIMIT 1",
    )
    .bind(scope.tenant_id())
    .bind(item_id)
    .bind(currency.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((p,)) = listed {
        return Ok(Some(p));
    }
    if own == currency {
        return Ok(Some(own_minor));
    }
    Ok(markets::converted(conn, scope, own_minor, own, currency)
        .await?
        .map(|(m, _)| m))
}

pub(crate) fn currency(code: &str) -> Result<Currency> {
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
    let customer_tag = tag(b.customer_tag.as_deref())?.filter(|t| !t.is_empty());
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let list = sqlx::query_as::<_, PriceList>(&format!(
        "INSERT INTO price_lists (id, tenant_id, name, currency, priority, active, customer_tag) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"
    ))
    .bind(PriceListId::new().as_str())
    .bind(tenant.id())
    .bind(&b.name)
    .bind(currency.as_str())
    .bind(b.priority.unwrap_or(0))
    .bind(b.active.unwrap_or(true))
    .bind(&customer_tag)
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

/// Rename, reprioritize, switch a list on or off, or give it to a
/// customer group (`pricing:write`).
/// Publishes `price_list.updated`.
#[patch("/api/price-lists/{id}", tag = "pricing")]
async fn update_price_list(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<PriceListChange>>,
) -> Result<Json<PriceList>> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let customer_tag = tag(c.customer_tag.as_deref())?;
    let current = load(&mut tx, &tenant, &id).await?;
    let list = sqlx::query_as::<_, PriceList>(&format!(
        "UPDATE price_lists SET name = COALESCE($3, name), priority = COALESCE($4, priority), \
         active = COALESCE($5, active), \
         customer_tag = CASE WHEN $6::text IS NULL THEN customer_tag ELSE NULLIF($6, '') END \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&c.name)
    .bind(c.priority)
    .bind(c.active)
    .bind(&customer_tag)
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
        "item_id, price_minor, compare_at_minor, min_quantity",
    )
    .scope_text("price_list_id", list.id)
    .sortable(["price_minor", "min_quantity"])
    .filter_text("item_id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Set prices in a list, up to 500 at once (`pricing:write`). Items are
/// variants or products without variants; `min_quantity` makes a price a
/// tier from that many units. An unknown item, or a compare-at price not
/// above the price, is a 422 and nothing changes.
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
        if p.price_minor < 0
            || p.compare_at_minor.is_some_and(|c| c <= p.price_minor)
            || p.min_quantity.is_some_and(|q| !(1..=100_000).contains(&q))
        {
            return Err(Error::unprocessable(format!(
                "{}: prices are at least 0, a compare-at price is above the price, \
                 and a tier starts at 1 to 100000 units",
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
             (tenant_id, price_list_id, item_id, price_minor, compare_at_minor, min_quantity) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (price_list_id, item_id, min_quantity) DO UPDATE \
             SET price_minor = EXCLUDED.price_minor, compare_at_minor = EXCLUDED.compare_at_minor",
        )
        .bind(tenant.id())
        .bind(&list.id)
        .bind(&item.id)
        .bind(p.price_minor)
        .bind(p.compare_at_minor)
        .bind(p.min_quantity.unwrap_or(1))
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
    Ok(Json(
        body.prices
            .into_iter()
            .map(|p| Price {
                min_quantity: Some(p.min_quantity.unwrap_or(1)),
                ..p
            })
            .collect(),
    ))
}

/// Remove an item's prices from a list (`pricing:write`), or only one
/// tier with `?min_quantity=`. Publishes `price_list.prices_changed`.
#[delete(
    "/api/price-lists/{id}/prices/{item_id}",
    tag = "pricing",
    status = 204
)]
async fn delete_price(
    db: Db,
    tenant: Tenant,
    Path((id, item_id)): Path<(String, String)>,
    params: ListParams,
) -> Result<StatusCode> {
    let tier = params
        .get("min_quantity")
        .map(|q| {
            q.parse::<i32>().map_err(|_| {
                Error::bad_request("min_quantity is a number").with_code("invalid_quantity")
            })
        })
        .transpose()?;
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let list = load(&mut tx, &tenant, &id).await?;
    let gone = sqlx::query(
        "DELETE FROM price_list_prices WHERE tenant_id = $1 AND price_list_id = $2 \
         AND item_id = $3 AND ($4::int IS NULL OR min_quantity = $4)",
    )
    .bind(tenant.id())
    .bind(&list.id)
    .bind(&item_id)
    .bind(tier)
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

/// What a buyer pays for one unit of an item in `?currency=`
/// (`pricing:read`), buying `?quantity=` (default 1) as a customer with
/// `?tags=` (comma separated; none for a guest): 404 when the item is not
/// sold in that currency.
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
    let quantity = match params.get("quantity") {
        None => 1,
        Some(q) => q.parse::<i64>().ok().filter(|q| *q >= 1).ok_or_else(|| {
            Error::bad_request("quantity is 1 or more").with_code("invalid_quantity")
        })?,
    };
    let tags: Vec<String> = params
        .get("tags")
        .unwrap_or("")
        .split(',')
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let price = resolve_for(
        &mut tx,
        &tenant,
        &item_id,
        currency,
        Buyer {
            tags: &tags,
            quantity,
        },
    )
    .await?;
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
