//! Fulfillments: shipping an order's lines.

use soat_core::id_kind;
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{load, Order, OrderId, ORDERS_WRITE};

id_kind!(FulfillmentId, FulfillmentKind, "ful");

/// A shipment of some of an order's lines.
#[model]
pub struct Fulfillment {
    /// e.g. `ful_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// `shipped`, `delivered` or `canceled`.
    pub status: String,
    pub carrier: String,
    pub tracking_number: String,
    pub tracking_url: String,
    pub lines: Vec<FulfillmentLine>,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub delivered_at: Option<String>,
    pub canceled_at: Option<String>,
}

/// Units of one order line in a shipment.
#[model]
#[derive(sqlx::FromRow)]
pub struct FulfillmentLine {
    pub line_id: String,
    pub item_id: String,
    pub quantity: i32,
    /// Where they left from; null for untracked items.
    pub location_id: Option<String>,
}

/// Units of a line to ship.
#[model]
pub struct ShipLine {
    pub line_id: String,
    #[validate(range(min = 1))]
    pub quantity: i64,
}

/// Body of a shipment.
#[model]
pub struct NewFulfillment {
    /// Defaults to everything not shipped yet.
    pub lines: Option<Vec<ShipLine>>,
    /// Where the units leave from; defaults, per item, to the location
    /// with the most on hand.
    pub location_id: Option<String>,
    #[validate(length(max = 50))]
    pub carrier: Option<String>,
    #[validate(length(max = 100))]
    pub tracking_number: Option<String>,
    #[validate(url, length(max = 500))]
    pub tracking_url: Option<String>,
}

/// Body of a tracking change.
#[model]
pub struct TrackingChange {
    #[validate(length(max = 50))]
    pub carrier: Option<String>,
    #[validate(length(max = 100))]
    pub tracking_number: Option<String>,
    #[validate(url, length(max = 500))]
    pub tracking_url: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        create_fulfillment,
        update_tracking,
        mark_delivered,
        cancel_fulfillment,
    ))
}

const COLUMNS: &str = "id, status, carrier, tracking_number, tracking_url, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
     to_char(delivered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
     to_char(canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

async fn lines_of(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<Vec<FulfillmentLine>> {
    Ok(sqlx::query_as::<_, FulfillmentLine>(
        "SELECT order_line_id AS line_id, item_id, quantity, location_id FROM fulfillment_lines \
         WHERE tenant_id = $1 AND fulfillment_id = $2 ORDER BY order_line_id",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

fn from_row(r: Row, lines: Vec<FulfillmentLine>) -> Fulfillment {
    Fulfillment {
        id: r.0,
        status: r.1,
        carrier: r.2,
        tracking_number: r.3,
        tracking_url: r.4,
        lines,
        created_at: r.5,
        delivered_at: r.6,
        canceled_at: r.7,
    }
}

/// An order's fulfillments, oldest first.
pub async fn of_order(
    conn: &mut PgConnection,
    scope: &impl Scope,
    order_id: &str,
) -> Result<Vec<Fulfillment>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM fulfillments WHERE tenant_id = $1 AND order_id = $2 ORDER BY id"
    ))
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let lines = lines_of(conn, scope, &r.0).await?;
        out.push(from_row(r, lines));
    }
    Ok(out)
}

/// Sets the order's fulfillment status from what shipped and publishes
/// `order.fulfilled` when everything did, else `order.updated`, if it
/// changed.
async fn sync(tx: &mut TenantTx, scope: &impl Scope, order_id: &str) -> Result<Order> {
    let (ordered, shipped): (i64, i64) = sqlx::query_as(
        "SELECT \
         COALESCE((SELECT SUM(quantity) FROM order_lines WHERE tenant_id = $1 AND order_id = $2), 0)::BIGINT, \
         COALESCE((SELECT SUM(fl.quantity) FROM fulfillment_lines fl \
                   JOIN fulfillments f ON f.id = fl.fulfillment_id \
                   WHERE f.tenant_id = $1 AND f.order_id = $2 AND f.status <> 'canceled'), 0)::BIGINT",
    )
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await?;
    let next = if shipped == 0 {
        "unfulfilled"
    } else if shipped >= ordered {
        "fulfilled"
    } else {
        "partially_fulfilled"
    };
    let changed = sqlx::query(
        "UPDATE orders SET fulfillment_status = $3 \
         WHERE tenant_id = $1 AND id = $2 AND fulfillment_status <> $3",
    )
    .bind(scope.tenant_id())
    .bind(order_id)
    .bind(next)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    let order = load(tx, scope, order_id)
        .await?
        .ok_or_else(|| Error::internal("order vanished"))?;
    if changed {
        let kind = if next == "fulfilled" {
            "order.fulfilled"
        } else {
            "order.updated"
        };
        scope.publish(tx, kind, &order).await?;
    }
    Ok(order)
}

/// Ship some or all of an open order's remaining units (`orders:write`):
/// takes them out of stock (recorded as `sold`) and lets go of the units
/// held for the order. More than is left of a line is a 422. Publishes
/// `fulfillment.created`, and `order.fulfilled` once all shipped.
#[post("/api/orders/{id}/fulfillments", tag = "orders", status = 201)]
async fn create_fulfillment(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(f)): Valid<Json<NewFulfillment>>,
) -> Result<(StatusCode, Json<Order>)> {
    let order_id = OrderId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let status: Option<(String,)> =
        sqlx::query_as("SELECT status FROM orders WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
            .bind(tenant.id())
            .bind(order_id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    let (status,) = status.ok_or_else(Error::not_found)?;
    if status != "open" {
        return Err(Error::conflict("a canceled order cannot ship").with_code("order_canceled"));
    }
    let location = match &f.location_id {
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
    // What is left to ship of each line.
    let left: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT l.id, l.item_id, (l.quantity - COALESCE((SELECT SUM(fl.quantity) \
           FROM fulfillment_lines fl JOIN fulfillments f ON f.id = fl.fulfillment_id \
           WHERE fl.order_line_id = l.id AND f.status <> 'canceled'), 0))::BIGINT \
         FROM order_lines l WHERE l.tenant_id = $1 AND l.order_id = $2 ORDER BY l.position",
    )
    .bind(tenant.id())
    .bind(order_id.as_str())
    .fetch_all(&mut *tx)
    .await?;
    let wanted: Vec<(String, String, i64)> = match &f.lines {
        None => left.into_iter().filter(|(_, _, q)| *q > 0).collect(),
        Some(lines) => {
            let mut out: Vec<(String, String, i64)> = Vec::new();
            for l in lines {
                let Some((_, item, rest)) = left.iter().find(|(id, _, _)| *id == l.line_id) else {
                    return Err(Error::unprocessable(format!("no line {}", l.line_id))
                        .with_code("unknown_line"));
                };
                if out.iter().any(|(id, _, _)| *id == l.line_id) {
                    return Err(
                        Error::unprocessable(format!("{} is listed twice", l.line_id))
                            .with_code("invalid_lines"),
                    );
                }
                if l.quantity > *rest {
                    return Err(Error::unprocessable(format!(
                        "only {rest} of {} left to ship",
                        l.line_id
                    ))
                    .with_code("too_many_units"));
                }
                out.push((l.line_id.clone(), item.clone(), l.quantity));
            }
            out
        }
    };
    if wanted.is_empty() {
        return Err(Error::unprocessable("nothing left to ship").with_code("nothing_to_ship"));
    }
    record(
        &mut tx,
        &tenant,
        order_id.as_str(),
        &wanted,
        location.as_deref(),
        [
            f.carrier.as_deref().unwrap_or("").trim(),
            f.tracking_number.as_deref().unwrap_or("").trim(),
            f.tracking_url.as_deref().unwrap_or(""),
        ],
        false,
    )
    .await?;
    let order = sync(&mut tx, &tenant, order_id.as_str()).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(order)))
}

/// Records a fulfillment of `wanted` (line, item, units), taking them out
/// of stock, and publishes `fulfillment.created`. `tracking` is carrier,
/// number and link; `delivered` records it as delivered at once.
#[allow(clippy::too_many_arguments)]
async fn record(
    tx: &mut TenantTx,
    scope: &impl Scope,
    order_id: &str,
    wanted: &[(String, String, i64)],
    location: Option<&str>,
    tracking: [&str; 3],
    delivered: bool,
) -> Result<Fulfillment> {
    let id = FulfillmentId::new();
    sqlx::query(
        "INSERT INTO fulfillments (id, tenant_id, order_id, carrier, tracking_number, \
         tracking_url, status, delivered_at) \
         VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $7 THEN 'delivered' ELSE 'shipped' END, \
                 CASE WHEN $7 THEN now() END)",
    )
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(order_id)
    .bind(tracking[0])
    .bind(tracking[1])
    .bind(tracking[2])
    .bind(delivered)
    .execute(&mut **tx)
    .await?;
    for (line, item, quantity) in wanted {
        let from =
            soat_inventory::stock::ship(tx, scope, item, *quantity, location, order_id).await?;
        sqlx::query(
            "INSERT INTO fulfillment_lines (tenant_id, fulfillment_id, order_line_id, item_id, \
             quantity, location_id) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(scope.tenant_id())
        .bind(id.as_str())
        .bind(line)
        .bind(item)
        .bind(*quantity as i32)
        .bind(from)
        .execute(&mut **tx)
        .await?;
    }
    let made = one(tx, scope, id.as_str()).await?;
    scope
        .publish(
            tx,
            "fulfillment.created",
            &serde_json::json!({ "order_id": order_id, "fulfillment": made }),
        )
        .await?;
    Ok(made)
}

/// Delivers what is left of an open order's digital lines (a fulfillment
/// with carrier `digital`, delivered at once), once it is paid. Does
/// nothing when there is none. Run it in the order's transaction.
pub(crate) async fn deliver_digital(
    tx: &mut TenantTx,
    scope: &impl Scope,
    order_id: &str,
) -> Result<()> {
    let left: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT l.id, l.item_id, (l.quantity - COALESCE((SELECT SUM(fl.quantity) \
           FROM fulfillment_lines fl JOIN fulfillments f ON f.id = fl.fulfillment_id \
           WHERE fl.order_line_id = l.id AND f.status <> 'canceled'), 0))::BIGINT AS rest \
         FROM order_lines l JOIN orders o ON o.id = l.order_id \
         WHERE l.tenant_id = $1 AND l.order_id = $2 AND NOT l.requires_shipping \
           AND o.status = 'open' ORDER BY l.position",
    )
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await?;
    let left: Vec<_> = left.into_iter().filter(|(_, _, q)| *q > 0).collect();
    if left.is_empty() {
        return Ok(());
    }
    record(tx, scope, order_id, &left, None, ["digital", "", ""], true).await?;
    sync(tx, scope, order_id).await?;
    Ok(())
}

async fn one(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Fulfillment> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM fulfillments WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let row = row.ok_or_else(Error::not_found)?;
    let lines = lines_of(conn, scope, &row.0).await?;
    Ok(from_row(row, lines))
}

/// A fulfillment of the shop, locked, with its order id and status.
pub(crate) async fn locked(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: &str,
) -> Result<(String, String, String)> {
    let id = FulfillmentId::parse(id).map_err(|_| Error::not_found())?;
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT order_id, status FROM fulfillments WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let (order, status) = row.ok_or_else(Error::not_found)?;
    Ok((id.to_string(), order, status))
}

pub(crate) async fn changed(
    tx: &mut TenantTx,
    tenant: &Tenant,
    id: &str,
    order: &str,
    kind: &str,
) -> Result<Order> {
    let f = one(tx, tenant, id).await?;
    tenant
        .publish(
            tx,
            kind,
            &serde_json::json!({ "order_id": order, "fulfillment": f }),
        )
        .await?;
    load(tx, tenant, order)
        .await?
        .ok_or_else(|| Error::internal("order vanished"))
}

/// Set a shipment's carrier and tracking (`orders:write`). Publishes
/// `fulfillment.updated`.
#[patch("/api/fulfillments/{id}", tag = "orders")]
async fn update_tracking(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(t)): Valid<Json<TrackingChange>>,
) -> Result<Json<Order>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let (id, order, _) = locked(&mut tx, &tenant, &id).await?;
    sqlx::query(
        "UPDATE fulfillments SET carrier = COALESCE($3, carrier), \
         tracking_number = COALESCE($4, tracking_number), \
         tracking_url = COALESCE($5, tracking_url) WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&id)
    .bind(t.carrier.as_deref().map(str::trim))
    .bind(t.tracking_number.as_deref().map(str::trim))
    .bind(&t.tracking_url)
    .execute(&mut *tx)
    .await?;
    let order = changed(&mut tx, &tenant, &id, &order, "fulfillment.updated").await?;
    tx.commit().await?;
    Ok(Json(order))
}

/// Record that a shipment arrived (`orders:write`). Publishes
/// `fulfillment.delivered`.
#[post("/api/fulfillments/{id}/delivered", tag = "orders")]
async fn mark_delivered(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Order>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let (id, order, status) = locked(&mut tx, &tenant, &id).await?;
    if status != "shipped" {
        return Err(
            Error::conflict(format!("a {status} shipment cannot be delivered"))
                .with_code("invalid_fulfillment_status"),
        );
    }
    sqlx::query(
        "UPDATE fulfillments SET status = 'delivered', delivered_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    let order = changed(&mut tx, &tenant, &id, &order, "fulfillment.delivered").await?;
    tx.commit().await?;
    Ok(Json(order))
}

/// Cancel a shipment that has not arrived, putting its units back where
/// they left from (`orders:write`). Publishes `fulfillment.canceled`.
#[post("/api/fulfillments/{id}/cancel", tag = "orders")]
async fn cancel_fulfillment(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Order>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let (id, order, status) = locked(&mut tx, &tenant, &id).await?;
    if status != "shipped" {
        return Err(
            Error::conflict(format!("a {status} shipment cannot be canceled"))
                .with_code("invalid_fulfillment_status"),
        );
    }
    for l in lines_of(&mut tx, &tenant, &id).await? {
        if let Some(location) = &l.location_id {
            soat_inventory::stock::restock(
                &mut tx,
                &tenant,
                &l.item_id,
                i64::from(l.quantity),
                location,
                &order,
            )
            .await?;
        }
    }
    sqlx::query(
        "UPDATE fulfillments SET status = 'canceled', canceled_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    changed(&mut tx, &tenant, &id, &order, "fulfillment.canceled").await?;
    let order = sync(&mut tx, &tenant, &order).await?;
    tx.commit().await?;
    Ok(Json(order))
}
