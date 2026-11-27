//! Selling on marketplaces: Amazon, eBay, Otto, Kaufland, Zalando, bol,
//! Cdiscount, Allegro, Etsy or any other.
//!
//! A **marketplace** (`/api/marketplaces`) holds how the shop sells there:
//! the currency, a markup on the shop's prices, units held back from the
//! stock offered, and whether every active product is offered or only the
//! listed ones. **Listings** (`PUT /api/marketplaces/{id}/listings`) give
//! an item its SKU or price there, or leave it out.
//!
//! A connector (an app, a plugin or a script with an API key holding
//! `marketplaces:sync`) does the talking to the marketplace:
//!
//! 1. it sends the **offers** (`GET /api/marketplaces/{id}/offers`: SKU,
//!    price and quantity of each item) to the marketplace;
//! 2. it brings the marketplace's **orders** in (`POST
//!    /api/marketplaces/{id}/orders`). Each becomes a SOAT order, paid
//!    (the marketplace took the money) and holding stock, so it shows in
//!    orders, stock, invoices and reports like any other. Bringing the
//!    same order in twice gives back the first;
//! 3. it tells the marketplace what changed (`GET
//!    /api/marketplaces/{id}/orders?pending=true`: shipped with tracking,
//!    or canceled) and marks it told (`POST .../orders/{external_id}/synced`);
//! 4. it cancels an order the marketplace canceled (`POST
//!    .../orders/{external_id}/cancel`).
//!
//! Prices, quantities and order states are worked out in
//! [`soat_core::marketplaces`].

use serde_json::json;
use soat_checkout::drafts::{self, DraftInput, DraftLineInput};
use soat_checkout::TaxProviders;
use soat_core::marketplaces::{self as rules, CHANNELS};
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_orders::fulfillments::Fulfillment;
use soat_orders::{Order, PostalAddress};
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

id_kind!(MarketplaceId, MarketplaceKind, "mkt");

/// See marketplaces, their listings and orders.
pub const MARKETPLACES_READ: Permission =
    Permission::new("marketplaces:read", "See marketplaces and their orders").for_staff();
/// Set up marketplaces and what is listed there.
pub const MARKETPLACES_MANAGE: Permission =
    Permission::new("marketplaces:manage", "Set up marketplaces and listings");
/// What a marketplace connector does: read offers, bring orders in and
/// report back.
pub const MARKETPLACES_SYNC: Permission = Permission::new(
    "marketplaces:sync",
    "Bring marketplace orders in and send offers, shipments and cancellations",
);

/// Most items offered to one marketplace.
const MAX_OFFERS: i64 = 2000;
/// Most lines in one marketplace order.
const MAX_LINES: usize = 200;

/// The marketplaces module.
pub struct Marketplaces;

impl Module for Marketplaces {
    fn name(&self) -> &'static str {
        "marketplaces"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "inventory",
            "pricing",
            "payments",
            "orders",
            "checkout",
        ]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[MARKETPLACES_READ, MARKETPLACES_MANAGE, MARKETPLACES_SYNC]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_marketplaces,
            create_marketplace,
            get_marketplace,
            update_marketplace,
            delete_marketplace,
            list_listings,
            put_listings,
            marketplace_offers,
            import_marketplace_order,
            list_marketplace_orders,
            mark_marketplace_order_synced,
            cancel_marketplace_order,
        ))
    }
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_marketplace")
}

// ------------------------------------------------------------ marketplaces

/// A marketplace the shop sells on.
#[model]
#[derive(sqlx::FromRow)]
pub struct Marketplace {
    /// e.g. `mkt_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub name: String,
    /// `amazon`, `ebay`, `otto`, `kaufland`, `zalando`, `bol`, `cdiscount`,
    /// `allegro`, `etsy` or `other`.
    pub channel: String,
    pub currency: String,
    /// Added to the shop's prices there, in basis points (1500 = 15 %).
    pub markup_bps: i64,
    /// Added after that, in minor units.
    pub markup_minor: i64,
    /// Units held back from the stock offered.
    pub stock_buffer: i64,
    /// Every active product is offered, unless a listing leaves it out;
    /// else only the listed ones.
    pub all_products: bool,
    pub active: bool,
    /// Orders brought in.
    pub orders: i64,
    /// Orders whose shipment or cancellation the marketplace was not told.
    pub pending: i64,
}

const COLUMNS: &str = "m.id, m.name, m.channel, m.currency, m.markup_bps, m.markup_minor, \
     m.stock_buffer, m.all_products, m.active, \
     (SELECT count(*) FROM marketplace_orders x WHERE x.tenant_id = m.tenant_id \
      AND x.marketplace_id = m.id) AS orders, \
     (SELECT count(*) FROM marketplace_orders x JOIN orders o ON o.id = x.order_id \
      WHERE x.tenant_id = m.tenant_id AND x.marketplace_id = m.id AND x.synced_state <> \
      CASE WHEN o.status = 'canceled' THEN 'canceled' \
           WHEN o.fulfillment_status = 'fulfilled' THEN 'shipped' \
           WHEN o.fulfillment_status = 'partially_fulfilled' THEN 'partially_shipped' \
           ELSE 'open' END) AS pending";

#[model]
pub struct MarketplaceList {
    pub items: Vec<Marketplace>,
}

/// A new marketplace.
#[model]
pub struct NewMarketplace {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub channel: String,
    /// Defaults to the shop's main currency.
    pub currency: Option<String>,
    #[validate(range(min = -9000, max = 100000))]
    pub markup_bps: Option<i64>,
    pub markup_minor: Option<i64>,
    #[validate(range(min = 0, max = 100000))]
    pub stock_buffer: Option<i64>,
    pub all_products: Option<bool>,
}

/// Changes to a marketplace.
#[model]
pub struct MarketplaceChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    #[validate(range(min = -9000, max = 100000))]
    pub markup_bps: Option<i64>,
    pub markup_minor: Option<i64>,
    #[validate(range(min = 0, max = 100000))]
    pub stock_buffer: Option<i64>,
    pub all_products: Option<bool>,
    pub active: Option<bool>,
}

async fn marketplace(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Marketplace> {
    let id = MarketplaceId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM marketplaces m WHERE m.tenant_id = $1 AND m.id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

fn taken(e: sqlx::Error) -> Error {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            Error::conflict("the shop has a marketplace of that name").with_code("name_taken")
        }
        _ => e.into(),
    }
}

/// The shop's marketplaces (`marketplaces:read`).
#[get("/api/marketplaces", tag = "marketplaces")]
async fn list_marketplaces(db: Db, tenant: Tenant) -> Result<Json<MarketplaceList>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_READ).await?;
    let items = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM marketplaces m WHERE m.tenant_id = $1 ORDER BY m.name, m.id \
         LIMIT 100"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(MarketplaceList { items }))
}

/// Start selling on a marketplace (`marketplaces:manage`). Publishes
/// `marketplace.created`.
#[post("/api/marketplaces", tag = "marketplaces", status = 201)]
async fn create_marketplace(
    db: Db,
    tenant: Tenant,
    Valid(Json(n)): Valid<Json<NewMarketplace>>,
) -> Result<(StatusCode, Json<Marketplace>)> {
    if !CHANNELS.contains(&n.channel.as_str()) {
        return Err(invalid(format!(
            "the channel is one of {}",
            CHANNELS.join(", ")
        )));
    }
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_MANAGE).await?;
    let currency = match n.currency.as_deref() {
        Some(c) => c.to_owned(),
        None => soat_catalog::storefront::main_currency(&mut tx, &tenant)
            .await?
            .unwrap_or_else(|| "EUR".to_owned()),
    };
    let currency = Currency::parse(&currency).map_err(|e| invalid(e.to_string()))?;
    let id = MarketplaceId::new();
    sqlx::query(
        "INSERT INTO marketplaces (id, tenant_id, name, channel, currency, markup_bps, \
         markup_minor, stock_buffer, all_products) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(n.name.trim())
    .bind(&n.channel)
    .bind(currency.as_str())
    .bind(n.markup_bps.unwrap_or(0))
    .bind(n.markup_minor.unwrap_or(0))
    .bind(n.stock_buffer.unwrap_or(0))
    .bind(n.all_products.unwrap_or(true))
    .execute(&mut *tx)
    .await
    .map_err(taken)?;
    let m = marketplace(&mut tx, &tenant, id.as_str()).await?;
    tenant.publish(&mut tx, "marketplace.created", &m).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

/// A marketplace (`marketplaces:read`).
#[get("/api/marketplaces/{id}", tag = "marketplaces")]
async fn get_marketplace(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Marketplace>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_READ).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Change how the shop sells on a marketplace, or pause it
/// (`marketplaces:manage`); a paused one offers nothing and takes no
/// orders. Publishes `marketplace.updated`.
#[patch("/api/marketplaces/{id}", tag = "marketplaces")]
async fn update_marketplace(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<MarketplaceChange>>,
) -> Result<Json<Marketplace>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_MANAGE).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    sqlx::query(
        "UPDATE marketplaces SET name = COALESCE($3, name), \
         markup_bps = COALESCE($4, markup_bps), markup_minor = COALESCE($5, markup_minor), \
         stock_buffer = COALESCE($6, stock_buffer), all_products = COALESCE($7, all_products), \
         active = COALESCE($8, active) WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(c.name.as_deref().map(str::trim))
    .bind(c.markup_bps)
    .bind(c.markup_minor)
    .bind(c.stock_buffer)
    .bind(c.all_products)
    .bind(c.active)
    .execute(&mut *tx)
    .await
    .map_err(taken)?;
    let m = marketplace(&mut tx, &tenant, &m.id).await?;
    tenant.publish(&mut tx, "marketplace.updated", &m).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Stop selling on a marketplace (`marketplaces:manage`). Its orders stay
/// as SOAT orders. Publishes `marketplace.deleted`.
#[delete("/api/marketplaces/{id}", tag = "marketplaces", status = 204)]
async fn delete_marketplace(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_MANAGE).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM marketplaces WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&m.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "marketplace.deleted",
            &json!({ "id": m.id, "name": m.name }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- listings

/// An item's listing on a marketplace.
#[model]
#[derive(sqlx::FromRow)]
pub struct Listing {
    pub item_id: String,
    pub sku: String,
    pub title: String,
    /// Its SKU there; blank for the shop's.
    pub external_sku: String,
    /// Its price there; null for the shop's price with the markup.
    pub price_minor: Option<i64>,
    /// False leaves it out.
    pub listed: bool,
}

#[model]
pub struct ListingList {
    pub items: Vec<Listing>,
}

/// A listing to set: an item by id or SKU.
#[model]
pub struct ListingInput {
    pub item_id: Option<String>,
    pub sku: Option<String>,
    #[validate(length(max = 100))]
    pub external_sku: Option<String>,
    #[validate(range(min = 0))]
    pub price_minor: Option<i64>,
    pub listed: Option<bool>,
}

/// Listings to set.
#[model]
pub struct ListingsInput {
    #[validate(length(min = 1, max = 500), nested)]
    pub listings: Vec<ListingInput>,
}

/// What the shop sells, one row per product without variants or variant,
/// as `item_id, sku, title`.
const SELLABLE: &str = "(SELECT v.id AS item_id, v.sku, \
       p.title || ' - ' || array_to_string(v.option_values, ' / ') AS title \
     FROM product_variants v JOIN products p ON p.id = v.product_id \
     WHERE v.tenant_id = $1 AND p.status = 'active' \
     UNION ALL \
     SELECT p.id, p.sku, p.title FROM products p WHERE p.tenant_id = $1 AND p.status = 'active' \
     AND NOT EXISTS (SELECT 1 FROM product_variants v WHERE v.tenant_id = p.tenant_id \
                     AND v.product_id = p.id)) AS i";

async fn listings(
    conn: &mut PgConnection,
    scope: &impl Scope,
    m: &Marketplace,
    only_offered: bool,
) -> Result<Vec<Listing>> {
    Ok(sqlx::query_as(&format!(
        "SELECT i.item_id, i.sku, i.title, COALESCE(l.external_sku, '') AS external_sku, \
         l.price_minor, COALESCE(l.listed, $3) AS listed FROM {SELLABLE} \
         LEFT JOIN marketplace_listings l ON l.tenant_id = $1 AND l.marketplace_id = $2 \
         AND l.item_id = i.item_id \
         WHERE ($4 = FALSE AND l.item_id IS NOT NULL) OR ($4 AND COALESCE(l.listed, $3)) \
         ORDER BY i.sku, i.item_id LIMIT $5"
    ))
    .bind(scope.tenant_id())
    .bind(&m.id)
    .bind(m.all_products)
    .bind(only_offered)
    .bind(MAX_OFFERS)
    .fetch_all(conn)
    .await?)
}

/// The listings set on a marketplace (`marketplaces:read`).
#[get("/api/marketplaces/{id}/listings", tag = "marketplaces")]
async fn list_listings(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<ListingList>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_READ).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let items = listings(&mut tx, &tenant, &m, false).await?;
    tx.commit().await?;
    Ok(Json(ListingList { items }))
}

/// Give items a SKU or price on a marketplace, list them or leave them
/// out (`marketplaces:manage`). Publishes `marketplace.listings_updated`.
#[put("/api/marketplaces/{id}/listings", tag = "marketplaces")]
async fn put_listings(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(input)): Valid<Json<ListingsInput>>,
) -> Result<Json<ListingList>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_MANAGE).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let mut set = Vec::with_capacity(input.listings.len());
    for l in &input.listings {
        let item = item(&mut tx, &tenant, l.item_id.as_deref(), l.sku.as_deref()).await?;
        sqlx::query(
            "INSERT INTO marketplace_listings (tenant_id, marketplace_id, item_id, external_sku, \
             price_minor, listed) VALUES ($1, $2, $3, COALESCE($4, ''), $5, COALESCE($6, TRUE)) \
             ON CONFLICT (tenant_id, marketplace_id, item_id) DO UPDATE SET \
             external_sku = COALESCE($4, marketplace_listings.external_sku), \
             price_minor = $5, listed = COALESCE($6, marketplace_listings.listed), \
             updated_at = now()",
        )
        .bind(tenant.id())
        .bind(&m.id)
        .bind(&item)
        .bind(l.external_sku.as_deref().map(str::trim))
        .bind(l.price_minor)
        .bind(l.listed)
        .execute(&mut *tx)
        .await?;
        set.push(item);
    }
    let items = listings(&mut tx, &tenant, &m, false).await?;
    tenant
        .publish(
            &mut tx,
            "marketplace.listings_updated",
            &json!({ "id": m.id, "item_ids": set }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(ListingList { items }))
}

/// An item the shop sells, by id or SKU.
async fn item(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: Option<&str>,
    sku: Option<&str>,
) -> Result<String> {
    match (id, sku) {
        (Some(i), _) => {
            let found: Option<(String,)> = sqlx::query_as(&format!(
                "SELECT i.item_id FROM {SELLABLE} WHERE i.item_id = $2"
            ))
            .bind(tenant.id())
            .bind(i)
            .fetch_optional(&mut **tx)
            .await?;
            found
                .map(|(i,)| i)
                .ok_or_else(|| invalid(format!("the shop sells no item {i}")))
        }
        (None, Some(sku)) => soat_catalog::item_by_sku(tx, tenant, sku.trim())
            .await?
            .map(|(i, _)| i)
            .ok_or_else(|| invalid(format!("no item has the SKU {sku}"))),
        (None, None) => Err(invalid("a line names an item_id or a sku")),
    }
}

// ------------------------------------------------------------------ offers

/// One item to offer on a marketplace.
#[model]
pub struct Offer {
    pub item_id: String,
    pub sku: String,
    /// Its SKU there: the listing's, else the shop's.
    pub external_sku: String,
    pub title: String,
    pub price_minor: i64,
    /// The regular price, when the item is on sale.
    pub compare_at_minor: Option<i64>,
    /// Units to offer: what is available less the stock buffer.
    pub quantity: i64,
}

#[model]
pub struct OfferList {
    pub currency: String,
    pub items: Vec<Offer>,
}

/// What to offer on a marketplace: each item with its SKU, price and the
/// units available (`marketplaces:sync`). Items not sold in the
/// marketplace's currency are left out; a paused marketplace offers
/// nothing.
#[get("/api/marketplaces/{id}/offers", tag = "marketplaces")]
async fn marketplace_offers(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<OfferList>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_SYNC).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let currency = Currency::parse(&m.currency).map_err(|e| invalid(e.to_string()))?;
    let mut items = Vec::new();
    if m.active {
        for l in listings(&mut tx, &tenant, &m, true).await? {
            let Some(p) = soat_pricing::resolve(&mut tx, &tenant, &l.item_id, currency).await?
            else {
                continue;
            };
            let (price, compare_at) = match l.price_minor {
                Some(own) => (own, None),
                None => (
                    rules::price(p.price_minor, m.markup_bps, m.markup_minor),
                    p.compare_at_minor
                        .filter(|c| *c > p.price_minor)
                        .map(|c| rules::price(c, m.markup_bps, m.markup_minor)),
                ),
            };
            let a =
                soat_inventory::reservations::availability(&mut tx, &tenant, &l.item_id).await?;
            items.push(Offer {
                external_sku: if l.external_sku.is_empty() {
                    l.sku.clone()
                } else {
                    l.external_sku
                },
                item_id: l.item_id,
                sku: l.sku,
                title: l.title,
                price_minor: price,
                compare_at_minor: compare_at,
                quantity: rules::quantity(a.tracked, a.available(), m.stock_buffer),
            });
        }
    }
    tx.commit().await?;
    Ok(Json(OfferList {
        currency: m.currency,
        items,
    }))
}

// ------------------------------------------------------------------ orders

/// A line of a marketplace order.
#[model]
pub struct ImportLine {
    pub item_id: Option<String>,
    /// The shop's SKU, or the listing's SKU on the marketplace.
    pub sku: Option<String>,
    #[validate(range(min = 1, max = 10000))]
    pub quantity: i64,
    /// What the buyer paid per unit there.
    #[validate(range(min = 0))]
    pub unit_price_minor: i64,
}

/// An order taken on a marketplace.
#[model]
pub struct ImportOrder {
    /// The marketplace's order number: an order is brought in once.
    #[validate(length(min = 1, max = 200))]
    pub external_id: String,
    /// Often a relay address of the marketplace's.
    #[validate(email)]
    pub email: Option<String>,
    #[validate(nested)]
    pub shipping_address: PostalAddress,
    #[validate(nested)]
    pub billing_address: Option<PostalAddress>,
    #[validate(length(min = 1, max = 200), nested)]
    pub lines: Vec<ImportLine>,
    #[validate(length(max = 100))]
    pub shipping_name: Option<String>,
    #[validate(range(min = 0))]
    pub shipping_price_minor: Option<i64>,
    /// Whether the marketplace has taken the money (most do); default true.
    pub paid: Option<bool>,
}

/// A marketplace order and its SOAT order.
#[model]
pub struct MarketplaceOrder {
    pub marketplace_id: String,
    pub external_id: String,
    pub order: Order,
    /// What to tell the marketplace: `open`, `partially_shipped`,
    /// `shipped` or `canceled`.
    pub state: String,
    /// What it was last told.
    pub synced_state: String,
    pub synced_at: Option<String>,
    /// Whether it should be told.
    pub pending: bool,
    /// The shipments, with their carriers and tracking numbers.
    pub fulfillments: Vec<Fulfillment>,
}

#[model]
pub struct MarketplaceOrderList {
    pub items: Vec<MarketplaceOrder>,
}

async fn shown(
    conn: &mut PgConnection,
    scope: &impl Scope,
    marketplace_id: &str,
    external_id: &str,
) -> Result<Option<MarketplaceOrder>> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT order_id, synced_state, to_char(synced_at AT TIME ZONE 'UTC', \
         'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM marketplace_orders \
         WHERE tenant_id = $1 AND marketplace_id = $2 AND external_id = $3",
    )
    .bind(scope.tenant_id())
    .bind(marketplace_id)
    .bind(external_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((order_id, synced_state, synced_at)) = row else {
        return Ok(None);
    };
    let order = soat_orders::load(conn, scope, &order_id)
        .await?
        .ok_or_else(Error::not_found)?;
    let fulfillments = soat_orders::fulfillments::of_order(conn, scope, &order_id)
        .await?
        .into_iter()
        .filter(|f| f.status != "canceled")
        .collect();
    let state = rules::state(&order.status, &order.fulfillment_status).to_owned();
    Ok(Some(MarketplaceOrder {
        marketplace_id: marketplace_id.to_owned(),
        external_id: external_id.to_owned(),
        pending: state != synced_state,
        state,
        synced_state,
        synced_at,
        order,
        fulfillments,
    }))
}

/// Bring an order taken on a marketplace in (`marketplaces:sync`): it
/// becomes a SOAT order at the marketplace's prices, holding stock and,
/// unless `paid` is false, paid. The same `external_id` again answers 200
/// with the order brought in first. An unknown SKU is a 422; too little
/// stock a 409 `insufficient_stock`. Publishes `order.placed`,
/// `payment.created` and `marketplace_order.imported`.
#[post("/api/marketplaces/{id}/orders", tag = "marketplaces", status = 201)]
async fn import_marketplace_order(
    db: Db,
    tenant: Tenant,
    taxes: State<TaxProviders>,
    Path(id): Path<String>,
    Valid(Json(o)): Valid<Json<ImportOrder>>,
) -> Result<(StatusCode, Json<MarketplaceOrder>)> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_SYNC).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let external = o.external_id.trim().to_owned();
    // Serialise imports of one marketplace, so an order comes in once.
    sqlx::query("SELECT 1 FROM marketplaces WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant.id())
        .bind(&m.id)
        .execute(&mut *tx)
        .await?;
    if let Some(known) = shown(&mut tx, &tenant, &m.id, &external).await? {
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(known)));
    }
    if !m.active {
        return Err(Error::conflict("the marketplace is paused").with_code("marketplace_paused"));
    }
    if o.lines.len() > MAX_LINES {
        return Err(invalid(format!("an order has 1 to {MAX_LINES} lines")));
    }
    let mut lines = Vec::with_capacity(o.lines.len());
    for l in &o.lines {
        let item = match (&l.item_id, &l.sku) {
            (None, Some(sku)) => match external_sku(&mut tx, &tenant, &m.id, sku.trim()).await? {
                Some(i) => i,
                None => item(&mut tx, &tenant, None, Some(sku)).await?,
            },
            (i, s) => item(&mut tx, &tenant, i.as_deref(), s.as_deref()).await?,
        };
        lines.push(DraftLineInput {
            item_id: item,
            quantity: l.quantity,
            unit_price_minor: Some(l.unit_price_minor),
        });
    }
    let input = DraftInput {
        customer_id: None,
        email: o.email.clone(),
        currency: m.currency.clone(),
        lines,
        billing_address: Some(
            o.billing_address
                .clone()
                .unwrap_or_else(|| o.shipping_address.clone()),
        ),
        shipping_address: Some(o.shipping_address.clone()),
        vat_number: None,
        shipping_name: Some(
            o.shipping_name
                .clone()
                .unwrap_or_else(|| format!("{} shipping", m.name)),
        ),
        shipping_price_minor: Some(o.shipping_price_minor.unwrap_or(0)),
        discount_minor: None,
        note: Some(format!("{} order {external}", m.name)),
    };
    let (placed, _token) = drafts::sell_at_counter(&mut tx, &tenant, &taxes, &input).await?;
    if o.paid.unwrap_or(true) && placed.total_minor > 0 {
        soat_payments::record(
            &mut tx,
            &tenant,
            &placed.id,
            "marketplace",
            &format!("{}:{external}", m.id),
            placed.total_minor,
            &placed.currency,
        )
        .await?;
        soat_orders::sync_payments(&mut tx, &tenant, &placed.id).await?;
    }
    sqlx::query(
        "INSERT INTO marketplace_orders (tenant_id, marketplace_id, external_id, order_id) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(&external)
    .bind(&placed.id)
    .execute(&mut *tx)
    .await?;
    let out = shown(&mut tx, &tenant, &m.id, &external)
        .await?
        .ok_or_else(|| Error::internal("the order just brought in is missing"))?;
    tenant
        .publish(
            &mut tx,
            "marketplace_order.imported",
            &json!({ "marketplace_id": m.id, "channel": m.channel, "external_id": external,
                     "order_id": out.order.id, "total_minor": out.order.total_minor }),
        )
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// The item a marketplace SKU stands for, by a listing.
async fn external_sku(
    tx: &mut TenantTx,
    tenant: &Tenant,
    marketplace_id: &str,
    sku: &str,
) -> Result<Option<String>> {
    let found: Option<(String,)> = sqlx::query_as(
        "SELECT item_id FROM marketplace_listings WHERE tenant_id = $1 AND marketplace_id = $2 \
         AND external_sku <> '' AND lower(external_sku) = lower($3) LIMIT 1",
    )
    .bind(tenant.id())
    .bind(marketplace_id)
    .bind(sku)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(found.map(|(i,)| i))
}

/// Which orders to list.
#[model]
pub struct OrdersQuery {
    /// Only those the marketplace should be told of.
    pub pending: Option<bool>,
}

/// A marketplace's orders, newest first, with what to tell it
/// (`marketplaces:sync`); `?pending=true` for those it was not told of.
#[get("/api/marketplaces/{id}/orders", tag = "marketplaces")]
async fn list_marketplace_orders(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Query(q): Query<OrdersQuery>,
) -> Result<Json<MarketplaceOrderList>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_SYNC).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let ids: Vec<(String,)> = sqlx::query_as(
        "SELECT x.external_id FROM marketplace_orders x JOIN orders o ON o.id = x.order_id \
         WHERE x.tenant_id = $1 AND x.marketplace_id = $2 AND (NOT $3 OR x.synced_state <> \
         CASE WHEN o.status = 'canceled' THEN 'canceled' \
              WHEN o.fulfillment_status = 'fulfilled' THEN 'shipped' \
              WHEN o.fulfillment_status = 'partially_fulfilled' THEN 'partially_shipped' \
              ELSE 'open' END) \
         ORDER BY x.created_at DESC, x.external_id LIMIT 100",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(q.pending.unwrap_or(false))
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::with_capacity(ids.len());
    for (external,) in ids {
        if let Some(o) = shown(&mut tx, &tenant, &m.id, &external).await? {
            items.push(o);
        }
    }
    tx.commit().await?;
    Ok(Json(MarketplaceOrderList { items }))
}

/// What the marketplace was told.
#[model]
pub struct Synced {
    /// `open`, `partially_shipped`, `shipped` or `canceled`.
    pub state: String,
}

/// Note that the marketplace was told an order's state
/// (`marketplaces:sync`). Publishes `marketplace_order.synced`.
#[post(
    "/api/marketplaces/{id}/orders/{external_id}/synced",
    tag = "marketplaces"
)]
async fn mark_marketplace_order_synced(
    db: Db,
    tenant: Tenant,
    Path((id, external_id)): Path<(String, String)>,
    Json(s): Json<Synced>,
) -> Result<Json<MarketplaceOrder>> {
    if !["open", "partially_shipped", "shipped", "canceled"].contains(&s.state.as_str()) {
        return Err(invalid(
            "the state is open, partially_shipped, shipped or canceled",
        ));
    }
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_SYNC).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let done = sqlx::query(
        "UPDATE marketplace_orders SET synced_state = $4, synced_at = now() \
         WHERE tenant_id = $1 AND marketplace_id = $2 AND external_id = $3",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(&external_id)
    .bind(&s.state)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    let out = shown(&mut tx, &tenant, &m.id, &external_id)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant
        .publish(
            &mut tx,
            "marketplace_order.synced",
            &json!({ "marketplace_id": m.id, "external_id": external_id,
                     "order_id": out.order.id, "state": s.state }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Why the marketplace canceled.
#[model]
pub struct CancelInput {
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

/// Cancel an order the marketplace canceled (`marketplaces:sync`): the
/// SOAT order is canceled and its stock released; the marketplace counts
/// as told. A shipped order is a 409 `not_cancelable`. Publishes
/// `order.canceled`.
#[post(
    "/api/marketplaces/{id}/orders/{external_id}/cancel",
    tag = "marketplaces"
)]
async fn cancel_marketplace_order(
    db: Db,
    tenant: Tenant,
    Path((id, external_id)): Path<(String, String)>,
    Valid(Json(c)): Valid<Json<CancelInput>>,
) -> Result<Json<MarketplaceOrder>> {
    let mut tx = tenant.begin_for(&db, &MARKETPLACES_SYNC).await?;
    let m = marketplace(&mut tx, &tenant, &id).await?;
    let known = shown(&mut tx, &tenant, &m.id, &external_id)
        .await?
        .ok_or_else(Error::not_found)?;
    if known.order.status != "canceled" {
        let reason = c
            .reason
            .as_deref()
            .map_or_else(|| format!("Canceled on {}", m.name), str::to_owned);
        soat_orders::cancel(&mut tx, &tenant, &known.order.id, &reason).await?;
    }
    sqlx::query(
        "UPDATE marketplace_orders SET synced_state = 'canceled', synced_at = now() \
         WHERE tenant_id = $1 AND marketplace_id = $2 AND external_id = $3",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(&external_id)
    .execute(&mut *tx)
    .await?;
    let out = shown(&mut tx, &tenant, &m.id, &external_id)
        .await?
        .ok_or_else(Error::not_found)?;
    tx.commit().await?;
    Ok(Json(out))
}
