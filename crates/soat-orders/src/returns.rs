//! Returns and refunds.
//!
//! A buyer (from their account) or staff asks to return shipped units; staff
//! receive them, putting them back into stock or not, and refund them
//! through the order's payments. Staff can also refund any amount of an
//! order without a return (a goodwill refund, a lost parcel).

use soat_core::id_kind;
use soat_customers::accounts::SignedIn;
use soat_payments::{Providers, RefundResult, PAYMENTS_REFUND};
use soat_tenancy::{Acting, Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{load, OrderId, ORDERS_READ, ORDERS_WRITE};

id_kind!(ReturnId, ReturnKind, "ret");

/// A return.
#[model]
pub struct Return {
    /// e.g. `ret_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub order_id: String,
    /// `requested`, `received`, `refunded` or `rejected`.
    pub status: String,
    pub reason: String,
    pub requested_by: Option<String>,
    pub lines: Vec<ReturnLine>,
    /// What these units are worth, tax included: the most a refund of
    /// them returns.
    pub value_minor: i64,
    pub refunded_minor: i64,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub received_at: Option<String>,
    pub closed_at: Option<String>,
}

/// Units of one order line in a return.
#[model]
#[derive(sqlx::FromRow)]
pub struct ReturnLine {
    pub line_id: String,
    pub item_id: String,
    pub quantity: i32,
    pub location_id: Option<String>,
}

/// Units of a line to return.
#[model]
pub struct ReturnLineInput {
    pub line_id: String,
    #[validate(range(min = 1))]
    pub quantity: i64,
}

/// Body of a return request.
#[model]
pub struct NewReturn {
    pub lines: Vec<ReturnLineInput>,
    #[validate(length(max = 1000))]
    pub reason: Option<String>,
}

/// Body of receiving a return.
#[model]
pub struct Receive {
    /// Put the units back into stock (default true).
    pub restock: Option<bool>,
    /// Where; defaults to where each line shipped from.
    pub location_id: Option<String>,
}

/// Body of a refund.
#[model]
pub struct NewRefund {
    /// Defaults to the return's value (for a return); required for an
    /// order refund.
    pub amount_minor: Option<i64>,
    /// For a return: also refund the order's shipping.
    #[serde(default)]
    pub include_shipping: bool,
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

/// Returns of an order.
#[model]
pub struct ReturnList {
    pub items: Vec<Return>,
}

/// The refunds made, one per payment touched.
#[model]
pub struct Refunded {
    pub refunds: Vec<RefundResult>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_returns,
        staff_return,
        customer_return,
        customer_returns,
        receive_return,
        reject_return,
        refund_return,
        refund_order,
    ))
}

const COLUMNS: &str = "id, order_id, status, reason, requested_by, refunded_minor, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
     to_char(received_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
     to_char(closed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

type Row = (
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    String,
    Option<String>,
    Option<String>,
);

/// What `quantity` units of an order line are worth, tax included.
/// What `quantity` of `ordered` units of a line cost the buyer: the units
/// less their share of the line's discount, plus their share of its tax
/// when it was added on top.
fn value_of(line: &LineWorth) -> i64 {
    let (quantity, ordered) = (i64::from(line.1), i64::from(line.2));
    if ordered == 0 {
        return 0;
    }
    let share =
        |amount: i64| (i128::from(amount) * i128::from(quantity) / i128::from(ordered)) as i64;
    let goods = line.0 * quantity - share(line.4);
    if line.5 {
        goods
    } else {
        goods + share(line.3)
    }
}

/// unit price, returned quantity, ordered quantity, line tax, line
/// discount, prices include tax.
type LineWorth = (i64, i32, i32, i64, i64, bool);

async fn build(conn: &mut PgConnection, scope: &impl Scope, r: Row) -> Result<Return> {
    let lines = sqlx::query_as::<_, ReturnLine>(
        "SELECT order_line_id AS line_id, item_id, quantity, location_id FROM return_lines \
         WHERE tenant_id = $1 AND return_id = $2 ORDER BY order_line_id",
    )
    .bind(scope.tenant_id())
    .bind(&r.0)
    .fetch_all(&mut *conn)
    .await?;
    let worth: Vec<LineWorth> = sqlx::query_as(
        "SELECT l.unit_price_minor, rl.quantity, l.quantity, l.tax_minor, l.discount_minor, \
         o.prices_include_tax \
         FROM return_lines rl JOIN order_lines l ON l.id = rl.order_line_id \
         JOIN orders o ON o.id = l.order_id \
         WHERE rl.tenant_id = $1 AND rl.return_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(&r.0)
    .fetch_all(&mut *conn)
    .await?;
    let value_minor = worth.iter().map(value_of).sum();
    Ok(Return {
        id: r.0,
        order_id: r.1,
        status: r.2,
        reason: r.3,
        requested_by: r.4,
        lines,
        value_minor,
        refunded_minor: r.5,
        created_at: r.6,
        received_at: r.7,
        closed_at: r.8,
    })
}

async fn one(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Return> {
    let id = ReturnId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM returns WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    build(conn, scope, row.ok_or_else(Error::not_found)?).await
}

async fn of_order(conn: &mut PgConnection, scope: &impl Scope, order: &str) -> Result<Vec<Return>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM returns WHERE tenant_id = $1 AND order_id = $2 ORDER BY id"
    ))
    .bind(scope.tenant_id())
    .bind(order)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(build(conn, scope, r).await?);
    }
    Ok(out)
}

/// Records a return request for shipped, not yet returned units and
/// publishes `return.requested`.
async fn request(
    tx: &mut TenantTx,
    scope: &impl Scope,
    order_id: &str,
    r: &NewReturn,
) -> Result<Return> {
    if r.lines.is_empty() {
        return Err(Error::unprocessable("name the lines to return").with_code("invalid_lines"));
    }
    sqlx::query("SELECT id FROM orders WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(scope.tenant_id())
        .bind(order_id)
        .execute(&mut **tx)
        .await?;
    // Per line: shipped units, less those in returns not rejected.
    let open: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT l.id, l.item_id, \
         (COALESCE((SELECT SUM(fl.quantity) FROM fulfillment_lines fl \
            JOIN fulfillments f ON f.id = fl.fulfillment_id \
            WHERE fl.order_line_id = l.id AND f.status <> 'canceled'), 0) \
          - COALESCE((SELECT SUM(rl.quantity) FROM return_lines rl \
            JOIN returns r ON r.id = rl.return_id \
            WHERE rl.order_line_id = l.id AND r.status <> 'rejected'), 0))::BIGINT \
         FROM order_lines l WHERE l.tenant_id = $1 AND l.order_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await?;
    let id = ReturnId::new();
    sqlx::query(
        "INSERT INTO returns (id, tenant_id, order_id, reason, requested_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(order_id)
    .bind(r.reason.as_deref().unwrap_or("").trim())
    .bind(scope.actor_id())
    .execute(&mut **tx)
    .await?;
    for l in &r.lines {
        let Some((_, item, left)) = open.iter().find(|(id, _, _)| *id == l.line_id) else {
            return Err(
                Error::unprocessable(format!("no line {}", l.line_id)).with_code("unknown_line")
            );
        };
        if l.quantity > *left {
            return Err(Error::unprocessable(format!(
                "only {left} shipped units of {} can be returned",
                l.line_id
            ))
            .with_code("too_many_units"));
        }
        sqlx::query(
            "INSERT INTO return_lines (tenant_id, return_id, order_line_id, item_id, quantity) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(scope.tenant_id())
        .bind(id.as_str())
        .bind(&l.line_id)
        .bind(item)
        .bind(l.quantity as i32)
        .execute(&mut **tx)
        .await
        .map_err(|_| {
            Error::unprocessable(format!("{} is listed twice", l.line_id))
                .with_code("invalid_lines")
        })?;
    }
    let made = one(tx, scope, id.as_str()).await?;
    scope.publish(tx, "return.requested", &made).await?;
    Ok(made)
}

/// An order's returns (`orders:read`).
#[get("/api/orders/{id}/returns", tag = "orders")]
async fn list_returns(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<ReturnList>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let order = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let items = of_order(&mut tx, &tenant, &order.id).await?;
    tx.commit().await?;
    Ok(Json(ReturnList { items }))
}

/// Open a return for shipped units (`orders:write`). More than shipped and
/// not yet returned is a 422. Publishes `return.requested`.
#[post("/api/orders/{id}/returns", tag = "orders", status = 201)]
async fn staff_return(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(r)): Valid<Json<NewReturn>>,
) -> Result<(StatusCode, Json<Return>)> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let order = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let made = request(&mut tx, &tenant, &order.id, &r).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// Ask to return units of one of the signed-in customer's orders.
/// Publishes `return.requested`.
#[post(
    "/api/storefront/account/orders/{id}/returns",
    tag = "storefront",
    status = 201
)]
async fn customer_return(
    db: Db,
    signed_in: SignedIn,
    Path(id): Path<String>,
    Valid(Json(r)): Valid<Json<NewReturn>>,
) -> Result<(StatusCode, Json<Return>)> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let order = load(&mut tx, &signed_in.shop, &id)
        .await?
        .filter(|o| o.customer_id.as_deref() == Some(customer.as_str()))
        .ok_or_else(Error::not_found)?;
    let acting = Acting {
        shop: &signed_in.shop,
        actor: Some(customer),
    };
    let made = request(&mut tx, &acting, &order.id, &r).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// The returns of one of the signed-in customer's orders.
#[get("/api/storefront/account/orders/{id}/returns", tag = "storefront")]
async fn customer_returns(
    db: Db,
    signed_in: SignedIn,
    Path(id): Path<String>,
) -> Result<Json<ReturnList>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let order = load(&mut tx, &signed_in.shop, &id)
        .await?
        .filter(|o| o.customer_id.as_deref() == Some(customer.as_str()))
        .ok_or_else(Error::not_found)?;
    let items = of_order(&mut tx, &signed_in.shop, &order.id).await?;
    tx.commit().await?;
    Ok(Json(ReturnList { items }))
}

async fn locked(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Return> {
    let rid = ReturnId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query("SELECT id FROM returns WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant.id())
        .bind(rid.as_str())
        .execute(&mut **tx)
        .await?;
    one(tx, tenant, id).await
}

fn wrong_status(r: &Return, action: &str) -> Error {
    Error::conflict(format!("a {} return cannot be {action}", r.status))
        .with_code("invalid_return_status")
}

/// Record that a return arrived (`orders:write`), putting its units back
/// into stock unless `restock` is false. Publishes `return.received`.
#[post("/api/returns/{id}/receive", tag = "orders")]
async fn receive_return(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(b)): Valid<Json<Receive>>,
) -> Result<Json<Return>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let r = locked(&mut tx, &tenant, &id).await?;
    if r.status != "requested" {
        return Err(wrong_status(&r, "received"));
    }
    let chosen = match &b.location_id {
        Some(l) => Some(
            soat_inventory::locations::load(&mut tx, &tenant, l)
                .await
                .map_err(|_| {
                    Error::unprocessable(format!("no location {l}")).with_code("unknown_location")
                })?
                .id,
        ),
        None => None,
    };
    if b.restock.unwrap_or(true) {
        for l in &r.lines {
            let shipped_from: Option<(Option<String>,)> = sqlx::query_as(
                "SELECT fl.location_id FROM fulfillment_lines fl \
                 JOIN fulfillments f ON f.id = fl.fulfillment_id \
                 WHERE fl.tenant_id = $1 AND fl.order_line_id = $2 AND f.status <> 'canceled' \
                 ORDER BY f.id DESC LIMIT 1",
            )
            .bind(tenant.id())
            .bind(&l.line_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(location) = chosen.clone().or(shipped_from.and_then(|(l,)| l)) else {
                continue; // untracked: nothing to put back
            };
            soat_inventory::stock::restock(
                &mut tx,
                &tenant,
                &l.item_id,
                i64::from(l.quantity),
                &location,
                &r.order_id,
            )
            .await?;
            sqlx::query(
                "UPDATE return_lines SET location_id = $4 \
                 WHERE tenant_id = $1 AND return_id = $2 AND order_line_id = $3",
            )
            .bind(tenant.id())
            .bind(&r.id)
            .bind(&l.line_id)
            .bind(&location)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(
        "UPDATE returns SET status = 'received', received_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .execute(&mut *tx)
    .await?;
    let r = one(&mut tx, &tenant, &r.id).await?;
    tenant.publish(&mut tx, "return.received", &r).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Turn a return request down (`orders:write`). Publishes
/// `return.rejected`.
#[post("/api/returns/{id}/reject", tag = "orders")]
async fn reject_return(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Return>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let r = locked(&mut tx, &tenant, &id).await?;
    if r.status != "requested" {
        return Err(wrong_status(&r, "rejected"));
    }
    sqlx::query(
        "UPDATE returns SET status = 'rejected', closed_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .execute(&mut *tx)
    .await?;
    let r = one(&mut tx, &tenant, &r.id).await?;
    tenant.publish(&mut tx, "return.rejected", &r).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Refund a received return through the order's payments (`orders:write`
/// and `payments:refund`): its value unless `amount_minor` says less,
/// plus shipping with `include_shipping`. Publishes `return.refunded` and
/// the payments' refund events.
#[post("/api/returns/{id}/refund", tag = "orders")]
async fn refund_return(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(b)): Valid<Json<NewRefund>>,
) -> Result<Json<Return>> {
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&ORDERS_WRITE)?;
    access.require(&PAYMENTS_REFUND)?;
    let r = locked(&mut tx, &tenant, &id).await?;
    if r.status != "received" {
        return Err(wrong_status(&r, "refunded"));
    }
    let mut most = r.value_minor;
    if b.include_shipping {
        let (shipping, shipping_tax, inclusive): (i64, i64, bool) = sqlx::query_as(
            "SELECT shipping_minor, shipping_tax_minor, prices_include_tax FROM orders \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant.id())
        .bind(&r.order_id)
        .fetch_one(&mut *tx)
        .await?;
        most += shipping + if inclusive { 0 } else { shipping_tax };
    }
    let amount = b.amount_minor.unwrap_or(most);
    if amount <= 0 || amount > most {
        return Err(
            Error::unprocessable(format!("refund 1 to {most} for this return"))
                .with_code("invalid_refund_amount"),
        );
    }
    let reason = b
        .reason
        .clone()
        .unwrap_or_else(|| format!("return {}", r.id));
    soat_payments::refund_reference(&mut tx, &providers, &tenant, &r.order_id, amount, &reason)
        .await?;
    sqlx::query(
        "UPDATE returns SET status = 'refunded', refunded_minor = $3, closed_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&r.id)
    .bind(amount)
    .execute(&mut *tx)
    .await?;
    let r = one(&mut tx, &tenant, &r.id).await?;
    tenant.publish(&mut tx, "return.refunded", &r).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Refund an amount of an order without a return, e.g. as goodwill
/// (`orders:write` and `payments:refund`). It comes out of the order's
/// payments, oldest first; more than they hold is a 422.
#[post("/api/orders/{id}/refunds", tag = "orders", status = 201)]
async fn refund_order(
    db: Db,
    tenant: Tenant,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(b)): Valid<Json<NewRefund>>,
) -> Result<(StatusCode, Json<Refunded>)> {
    let id = OrderId::parse(&id).map_err(|_| Error::not_found())?;
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&ORDERS_WRITE)?;
    access.require(&PAYMENTS_REFUND)?;
    load(&mut tx, &tenant, id.as_str())
        .await?
        .ok_or_else(Error::not_found)?;
    let amount = b.amount_minor.ok_or_else(|| {
        Error::unprocessable("give amount_minor").with_code("invalid_refund_amount")
    })?;
    let refunds = soat_payments::refund_reference(
        &mut tx,
        &providers,
        &tenant,
        id.as_str(),
        amount,
        b.reason.as_deref().unwrap_or(""),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(Refunded { refunds })))
}

#[cfg(test)]
mod tests {
    use super::value_of;

    #[test]
    fn values() {
        assert_eq!(value_of(&(1190, 1, 2, 380, 0, true)), 1190);
        assert_eq!(value_of(&(1000, 1, 2, 380, 0, false)), 1190);
        assert_eq!(value_of(&(1000, 2, 3, 100, 0, false)), 2066);
        // 10% off two units: one returned gives back 900 plus its tax.
        assert_eq!(value_of(&(1000, 1, 2, 342, 200, false)), 900 + 171);
        assert_eq!(value_of(&(1000, 0, 2, 342, 200, false)), 0);
    }
}
