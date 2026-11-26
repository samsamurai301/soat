//! Order endpoints: staff, signed-in customers and guests with a token.

use soat_customers::accounts::SignedIn;
use soat_tenancy::{Scope, Shop, Tenant, TenantListing, TenantTx};
use zapline::auth::password::hash_token;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::extract::FromRequestParts;
use zapline::prelude::*;

use crate::{load, Order, OrderId, ORDERS_READ, ORDERS_WRITE, TOKEN_HEADER};

/// An order in a list.
#[model]
#[derive(sqlx::FromRow)]
pub struct OrderSummary {
    pub id: String,
    pub number: i64,
    pub email: String,
    pub customer_id: Option<String>,
    pub currency: String,
    pub total_minor: i64,
    pub status: String,
    pub financial_status: String,
    pub fulfillment_status: String,
    /// RFC 3339, UTC.
    pub placed_at: String,
}

const SUMMARY: &str = "id, number, email, customer_id, currency, total_minor, status, \
     financial_status, fulfillment_status, \
     to_char(placed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS placed_at";

/// Body of an order change.
#[model]
pub struct OrderChange {
    /// Staff's note.
    #[validate(length(max = 5000))]
    pub note: Option<String>,
}

/// Body of a cancellation.
#[model]
pub struct Cancel {
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_orders,
        get_order,
        update_order,
        cancel_order,
        my_orders,
        my_order,
        guest_order,
    ))
}

/// The shop's orders, newest first (`orders:read`). Filter with
/// `?status=`, `?financial_status=`, `?fulfillment_status=` or
/// `?customer_id=`; `?q=` searches emails and order numbers.
#[get("/api/orders", tag = "orders")]
async fn list_orders(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<OrderSummary>>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let page = TenantListing::new("orders", SUMMARY)
        .sortable(["number", "total_minor", "placed_at"])
        .default_sort("-number")
        .search(["email", "number::text"])
        .filter_text("status")
        .filter_text("financial_status")
        .filter_text("fulfillment_status")
        .filter_text("customer_id")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// One order with its lines (`orders:read`).
#[get("/api/orders/{id}", tag = "orders")]
async fn get_order(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Order>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let order = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    order.map(Json).ok_or_else(Error::not_found)
}

/// Change an order's note (`orders:write`). Publishes `order.updated`.
#[patch("/api/orders/{id}", tag = "orders")]
async fn update_order(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<OrderChange>>,
) -> Result<Json<Order>> {
    let id = OrderId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let found =
        sqlx::query("UPDATE orders SET note = COALESCE($3, note) WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(id.as_str())
            .bind(c.note.as_deref().map(str::trim))
            .execute(&mut *tx)
            .await?;
    if found.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    let order = load(&mut tx, &tenant, id.as_str())
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "order.updated", &order).await?;
    tx.commit().await?;
    Ok(Json(order))
}

/// Cancel an open, unfulfilled order and release the stock held for it
/// (`orders:write`). Payments are not touched: refund them separately.
/// Publishes `order.canceled`.
#[post("/api/orders/{id}/cancel", tag = "orders")]
async fn cancel_order(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<Cancel>>,
) -> Result<Json<Order>> {
    let id = OrderId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT status, fulfillment_status FROM orders \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let (status, fulfillment) = row.ok_or_else(Error::not_found)?;
    if status != "open" || fulfillment != "unfulfilled" {
        return Err(
            Error::conflict("only open, unfulfilled orders can be canceled")
                .with_code("not_cancelable"),
        );
    }
    sqlx::query(
        "UPDATE orders SET status = 'canceled', canceled_at = now(), cancel_reason = $3 \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(c.reason.as_deref().unwrap_or("").trim())
    .execute(&mut *tx)
    .await?;
    soat_inventory::reservations::release(&mut tx, &tenant, id.as_str()).await?;
    let order = load(&mut tx, &tenant, id.as_str())
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "order.canceled", &order).await?;
    tx.commit().await?;
    Ok(Json(order))
}

/// The signed-in customer's orders, newest first.
#[get("/api/storefront/account/orders", tag = "storefront")]
async fn my_orders(
    db: Db,
    signed_in: SignedIn,
    params: ListParams,
) -> Result<Json<Paged<OrderSummary>>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let page = customer_orders(&mut tx, &customer, &params).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Customer `customer`'s orders, newest first, in the shop's transaction.
///
/// # Errors
///
/// Database errors.
pub async fn customer_orders(
    conn: &mut PgConnection,
    customer: &str,
    params: &ListParams,
) -> Result<Paged<OrderSummary>> {
    TenantListing::new("orders", SUMMARY)
        .scope_text("customer_id", customer.to_owned())
        .sortable(["number"])
        .default_sort("-number")
        .fetch(conn, params)
        .await
}

/// Order `id` if it is customer `customer`'s, in the shop's transaction.
///
/// # Errors
///
/// Database errors.
pub async fn customer_order(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
    id: &str,
) -> Result<Option<Order>> {
    Ok(load(conn, scope, id)
        .await?
        .filter(|o| o.customer_id.as_deref() == Some(customer)))
}

/// One of the signed-in customer's orders; anyone else's is a 404.
#[get("/api/storefront/account/orders/{id}", tag = "storefront")]
async fn my_order(db: Db, signed_in: SignedIn, Path(id): Path<String>) -> Result<Json<Order>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let order = customer_order(&mut tx, &signed_in.shop, &customer, &id).await?;
    tx.commit().await?;
    order.map(Json).ok_or_else(Error::not_found)
}

/// A guest holding an order's token (`x-soat-order-token`), for
/// storefront routes about one order.
pub struct OrderToken {
    pub shop: Shop,
    hash: String,
}

impl OrderToken {
    /// The holder of `token` in `shop`, e.g. from a cookie.
    #[must_use]
    pub fn new(shop: Shop, token: &str) -> Self {
        Self {
            shop,
            hash: hash_token(token),
        }
    }

    /// Opens the shop's transaction for order `id` if the token is its;
    /// otherwise a 404. Returns the transaction and the order's id.
    ///
    /// # Errors
    ///
    /// `404` for an unknown order or a wrong token; database errors.
    pub async fn open(&self, db: &Db, id: &str) -> Result<(TenantTx, String)> {
        let id = OrderId::parse(id).map_err(|_| Error::not_found())?;
        let mut tx = self.shop.begin(db).await?;
        let ok: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM orders WHERE tenant_id = $1 AND id = $2 AND token_hash = $3",
        )
        .bind(self.shop.id())
        .bind(id.as_str())
        .bind(&self.hash)
        .fetch_optional(&mut *tx)
        .await?;
        match ok {
            Some((id,)) => Ok((tx, id)),
            None => Err(Error::not_found()),
        }
    }
}

impl FromRequestParts for OrderToken {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let shop = Shop::from_request_parts(req).await?;
        let token = req
            .headers()
            .get(TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::unauthorized().with_code("missing_order_token"))?;
        Ok(Self {
            shop,
            hash: hash_token(token),
        })
    }
}

/// An order, for the guest holding its token (`x-soat-order-token`, given
/// when it was placed); a wrong token is a 404.
#[get("/api/storefront/orders/{id}", tag = "storefront")]
async fn guest_order(db: Db, holder: OrderToken, Path(id): Path<String>) -> Result<Json<Order>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let order = load(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    order.map(Json).ok_or_else(Error::not_found)
}
