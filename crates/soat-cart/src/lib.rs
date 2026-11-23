//! Carts.
//!
//! A cart has a currency and lines of stock items (variants, or products
//! without variants) with quantities. Prices are never stored: every read
//! resolves each line's current price in the cart's currency
//! ([`soat_pricing::resolve`]) and checks stock
//! ([`soat_inventory::reservations::availability`]), flagging lines that
//! can no longer be bought. Stock is only reserved at checkout (C9).
//!
//! Shoppers use the [`storefront`] endpoints, holding the cart by its
//! token; staff see carts (for abandoned-cart follow-up) here.

pub mod storefront;

use soat_catalog::stock_item;
use soat_core::{id_kind, Currency, Permission};
use soat_inventory::reservations::availability;
use soat_module::Module;
use soat_tenancy::{Scope, Tenant, TenantListing};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(CartId, CartKind, "cart");
id_kind!(LineId, LineKind, "line");

/// Most lines in one cart.
pub const MAX_LINES: i64 = 100;
/// Most units of one line.
pub const MAX_QUANTITY: i64 = 999;

/// See the shop's carts.
pub const CARTS_READ: Permission = Permission::new(
    "carts:read",
    "See the shop's carts, including abandoned ones",
)
.for_staff();

/// The cart module.
pub struct Carts;

impl Module for Carts {
    fn name(&self) -> &'static str {
        "cart"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "inventory",
            "pricing",
            "customers",
        ]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[CARTS_READ]
    }

    fn mount(&self, app: App) -> App {
        app.mount((list_carts, get_cart)).plugin(storefront::mount)
    }
}

/// A cart with today's prices.
#[model]
pub struct CartView {
    /// e.g. `cart_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub currency: String,
    pub email: String,
    /// The signed-in customer it belongs to, if any.
    pub customer_id: Option<String>,
    /// `open`, or `completed` once it became an order.
    pub status: String,
    pub lines: Vec<CartLine>,
    /// Units across all lines.
    pub item_count: i64,
    /// Sum of the lines that can be bought, before tax, shipping and
    /// discounts.
    pub subtotal_minor: i64,
    /// Whether every line can be bought as it is.
    pub ready: bool,
}

/// One line of a cart.
#[model]
pub struct CartLine {
    /// `line_...`.
    pub id: String,
    pub item_id: String,
    pub product_id: Option<String>,
    pub sku: Option<String>,
    pub title: Option<String>,
    pub quantity: i64,
    pub unit_price_minor: Option<i64>,
    pub compare_at_minor: Option<i64>,
    pub line_total_minor: Option<i64>,
    /// Why the line cannot be bought: `unavailable` (the item is gone),
    /// `not_sold_in_currency` or `insufficient_stock`; null when fine.
    pub issue: Option<String>,
}

/// A cart row as staff list them.
#[model]
#[derive(sqlx::FromRow)]
pub struct CartSummary {
    pub id: String,
    pub currency: String,
    pub email: String,
    pub customer_id: Option<String>,
    pub status: String,
    pub line_count: i64,
    /// RFC 3339, UTC.
    pub updated_at: String,
}

/// The cart with live prices and stock, or 404. Run it in the shop's
/// transaction.
pub async fn view(conn: &mut PgConnection, scope: &impl Scope, cart_id: &str) -> Result<CartView> {
    let row: Option<(String, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, currency, email, customer_id, status FROM carts \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(scope.tenant_id())
    .bind(cart_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (id, currency, email, customer_id, status) = row.ok_or_else(Error::not_found)?;
    let lines: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT id, item_id, quantity FROM cart_lines WHERE tenant_id = $1 AND cart_id = $2 \
         ORDER BY id",
    )
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    let code = Currency::parse(&currency).map_err(|e| Error::internal(e.to_string()))?;
    let mut out = Vec::with_capacity(lines.len());
    let (mut subtotal, mut count, mut ready) = (0i64, 0i64, true);
    for (line_id, item_id, quantity) in lines {
        let quantity = i64::from(quantity);
        count += quantity;
        let mut line = CartLine {
            id: line_id,
            item_id: item_id.clone(),
            product_id: None,
            sku: None,
            title: None,
            quantity,
            unit_price_minor: None,
            compare_at_minor: None,
            line_total_minor: None,
            issue: None,
        };
        // A product that gained variants since is no longer an item.
        let item = stock_item(conn, scope, &item_id).await.ok().flatten();
        let Some(item) = item else {
            line.issue = Some("unavailable".to_owned());
            ready = false;
            out.push(line);
            continue;
        };
        line.product_id = Some(item.product_id);
        line.sku = Some(item.sku);
        line.title = Some(item.title);
        match soat_pricing::resolve(conn, scope, &item_id, code).await? {
            None => line.issue = Some("not_sold_in_currency".to_owned()),
            Some(price) => {
                let total = price
                    .price_minor
                    .checked_mul(quantity)
                    .ok_or_else(|| Error::unprocessable("line total too large"))?;
                line.unit_price_minor = Some(price.price_minor);
                line.compare_at_minor = price.compare_at_minor;
                line.line_total_minor = Some(total);
                subtotal = subtotal
                    .checked_add(total)
                    .ok_or_else(|| Error::unprocessable("cart total too large"))?;
                if !availability(conn, scope, &item_id)
                    .await?
                    .can_sell(quantity)
                {
                    line.issue = Some("insufficient_stock".to_owned());
                }
            }
        }
        ready &= line.issue.is_none();
        out.push(line);
    }
    Ok(CartView {
        id,
        currency,
        email,
        customer_id,
        status,
        ready: ready && !out.is_empty(),
        lines: out,
        item_count: count,
        subtotal_minor: subtotal,
    })
}

/// The shop's carts, most recently changed first (`carts:read`). Filter
/// with `?status=open` or `?customer_id=`; `?q=` searches emails.
#[get("/api/carts", tag = "carts")]
async fn list_carts(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<CartSummary>>> {
    let mut tx = tenant.begin_for(&db, &CARTS_READ).await?;
    let page = TenantListing::new(
        "(SELECT c.*, (SELECT COUNT(*) FROM cart_lines l WHERE l.cart_id = c.id) AS line_count \
          FROM carts c) AS carts",
        "id, currency, email, customer_id, status, line_count, \
         to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at",
    )
    .sortable(["updated_at"])
    .default_sort("-updated_at")
    .search(["email"])
    .filter_text("status")
    .filter_text("customer_id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// One cart with today's prices (`carts:read`).
#[get("/api/carts/{id}", tag = "carts")]
async fn get_cart(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<CartView>> {
    let id = CartId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &CARTS_READ).await?;
    let cart = view(&mut tx, &tenant, id.as_str()).await?;
    tx.commit().await?;
    Ok(Json(cart))
}
