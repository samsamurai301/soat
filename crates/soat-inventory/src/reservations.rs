//! Stock reservations: units held for a cart or an order.
//!
//! A reservation names its `reference` (a cart or order id) and may expire.
//! Expired reservations stop counting at once and are deleted the next time
//! the item is reserved. Reserving locks the item, so two carts cannot both
//! take the last unit. Carts and checkout (C3, C9) call [`reserve`] and
//! [`release`]; the API below serves headless and external carts.

use soat_core::id_kind;
use soat_tenancy::{Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{INVENTORY_READ, INVENTORY_WRITE};

id_kind!(ReservationId, ReservationKind, "rsv");

/// Longest a reservation may be asked to last, in seconds (7 days).
pub const MAX_TTL_SECS: i64 = 7 * 24 * 3600;

/// Units held for a reference.
#[model]
#[derive(sqlx::FromRow)]
pub struct Reservation {
    /// `rsv_...`.
    pub id: String,
    pub item_id: String,
    pub quantity: i64,
    /// The cart or order holding the units.
    pub reference: String,
    /// RFC 3339, or null to hold until released.
    pub expires_at: Option<String>,
}

/// Body of a reservation request.
#[model]
pub struct NewReservation {
    pub item_id: String,
    #[validate(range(min = 1, max = 1000000))]
    pub quantity: i64,
    #[validate(length(min = 1, max = 200))]
    pub reference: String,
    /// Seconds until it lapses, at most 7 days; absent holds until released.
    #[validate(range(min = 1, max = 604800))]
    pub ttl_seconds: Option<i64>,
}

/// Body of a release by reference.
#[model]
pub struct Release {
    #[validate(length(min = 1, max = 200))]
    pub reference: String,
}

/// How much of an item can be sold now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    pub on_hand: i64,
    pub reserved: i64,
    pub tracked: bool,
    pub allow_backorder: bool,
}

impl Availability {
    /// On hand minus reserved, never below zero.
    pub fn available(&self) -> i64 {
        (self.on_hand - self.reserved).max(0)
    }

    /// Whether `quantity` more units can be promised.
    pub fn can_sell(&self, quantity: i64) -> bool {
        !self.tracked || self.allow_backorder || self.available() >= quantity
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        create_reservation,
        list_reservations,
        delete_reservation,
        release_reference,
    ))
}

const COLUMNS: &str = "id, item_id, quantity, reference, \
     to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at";

/// The item's stock and active reservations.
pub async fn availability(
    conn: &mut PgConnection,
    tenant: &Tenant,
    item_id: &str,
) -> Result<Availability> {
    let (on_hand, reserved, tracked, allow_backorder): (i64, i64, bool, bool) = sqlx::query_as(
        "SELECT \
         COALESCE((SELECT SUM(on_hand) FROM inventory_levels \
                   WHERE tenant_id = $1 AND item_id = $2), 0)::BIGINT, \
         COALESCE((SELECT SUM(quantity) FROM inventory_reservations \
                   WHERE tenant_id = $1 AND item_id = $2 \
                   AND (expires_at IS NULL OR expires_at > now())), 0)::BIGINT, \
         COALESCE((SELECT tracked FROM inventory_items WHERE tenant_id = $1 AND item_id = $2), TRUE), \
         COALESCE((SELECT allow_backorder FROM inventory_items \
                   WHERE tenant_id = $1 AND item_id = $2), FALSE)",
    )
    .bind(tenant.id())
    .bind(item_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Availability {
        on_hand,
        reserved,
        tracked,
        allow_backorder,
    })
}

/// Holds `quantity` of an item for `reference`, for `ttl_seconds` or until
/// released. 409 `insufficient_stock` when a tracked item without
/// backorders has fewer available. Publishes `inventory.reserved`.
pub async fn reserve(
    tx: &mut TenantTx,
    tenant: &Tenant,
    item_id: &str,
    quantity: i64,
    reference: &str,
    ttl_seconds: Option<i64>,
) -> Result<Reservation> {
    // Serialize reservations of this item on its settings row.
    sqlx::query(
        "INSERT INTO inventory_items (tenant_id, item_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(tenant.id())
    .bind(item_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("SELECT 1 FROM inventory_items WHERE tenant_id = $1 AND item_id = $2 FOR UPDATE")
        .bind(tenant.id())
        .bind(item_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "DELETE FROM inventory_reservations \
         WHERE tenant_id = $1 AND item_id = $2 AND expires_at <= now()",
    )
    .bind(tenant.id())
    .bind(item_id)
    .execute(&mut **tx)
    .await?;
    let now = availability(tx, tenant, item_id).await?;
    if !now.can_sell(quantity) {
        return Err(
            Error::conflict(format!("only {} available", now.available()))
                .with_code("insufficient_stock"),
        );
    }
    let reservation = sqlx::query_as::<_, Reservation>(&format!(
        "INSERT INTO inventory_reservations (id, tenant_id, item_id, quantity, reference, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6)) RETURNING {COLUMNS}"
    ))
    .bind(ReservationId::new().as_str())
    .bind(tenant.id())
    .bind(item_id)
    .bind(quantity)
    .bind(reference)
    .bind(ttl_seconds.map(|s| s as f64))
    .fetch_one(&mut **tx)
    .await?;
    tenant
        .publish(tx, "inventory.reserved", &reservation)
        .await?;
    Ok(reservation)
}

/// Releases every reservation of `reference` and returns how many units
/// were freed. Publishes `inventory.released` when any were.
pub async fn release(tx: &mut TenantTx, tenant: &Tenant, reference: &str) -> Result<i64> {
    let freed: Vec<(String, i64)> = sqlx::query_as(
        "DELETE FROM inventory_reservations WHERE tenant_id = $1 AND reference = $2 \
         RETURNING item_id, quantity",
    )
    .bind(tenant.id())
    .bind(reference)
    .fetch_all(&mut **tx)
    .await?;
    let units = freed.iter().map(|(_, q)| q).sum();
    if !freed.is_empty() {
        tenant
            .publish(
                tx,
                "inventory.released",
                &serde_json::json!({ "reference": reference, "units": units }),
            )
            .await?;
    }
    Ok(units)
}

/// Hold units of an item for a cart or order (`inventory:write`). Fewer
/// available than asked is a 409 `insufficient_stock` (untracked items and
/// backorders always succeed). Publishes `inventory.reserved`.
#[post("/api/inventory/reservations", tag = "inventory", status = 201)]
async fn create_reservation(
    db: Db,
    tenant: Tenant,
    Valid(Json(r)): Valid<Json<NewReservation>>,
) -> Result<(StatusCode, Json<Reservation>)> {
    if r.ttl_seconds.is_some_and(|t| t > MAX_TTL_SECS) {
        return Err(Error::unprocessable("ttl_seconds is at most 7 days").with_code("invalid_ttl"));
    }
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let item = soat_catalog::stock_item(&mut tx, &tenant, &r.item_id)
        .await?
        .ok_or_else(|| {
            Error::unprocessable(format!("no stock item {}", r.item_id)).with_code("unknown_item")
        })?;
    let reservation = reserve(
        &mut tx,
        &tenant,
        &item.id,
        r.quantity,
        &r.reference,
        r.ttl_seconds,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(reservation)))
}

/// Active reservations, newest first (`inventory:read`). Filter with
/// `?reference=` or `?item_id=`.
#[get("/api/inventory/reservations", tag = "inventory")]
async fn list_reservations(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Reservation>>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let page = TenantListing::new(
        "(SELECT * FROM inventory_reservations \
          WHERE expires_at IS NULL OR expires_at > now()) AS active",
        COLUMNS,
    )
    .default_sort("-id")
    .filter_text("reference")
    .filter_text("item_id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Release one reservation (`inventory:write`). Publishes
/// `inventory.released`.
#[delete("/api/inventory/reservations/{id}", tag = "inventory", status = 204)]
async fn delete_reservation(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = ReservationId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let gone: Option<(String, i64)> = sqlx::query_as(
        "DELETE FROM inventory_reservations WHERE tenant_id = $1 AND id = $2 \
         RETURNING reference, quantity",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((reference, units)) = gone else {
        return Err(Error::not_found());
    };
    tenant
        .publish(
            &mut tx,
            "inventory.released",
            &serde_json::json!({ "reference": reference, "units": units }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Release every reservation of a cart or order (`inventory:write`).
/// Releasing a reference that holds nothing is fine.
#[post("/api/inventory/reservations/release", tag = "inventory", status = 204)]
async fn release_reference(
    db: Db,
    tenant: Tenant,
    Valid(Json(r)): Valid<Json<Release>>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    release(&mut tx, &tenant, &r.reference).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_rules() {
        let a = Availability {
            on_hand: 5,
            reserved: 3,
            tracked: true,
            allow_backorder: false,
        };
        assert_eq!(a.available(), 2);
        assert!(a.can_sell(2));
        assert!(!a.can_sell(3));
        assert!(Availability {
            allow_backorder: true,
            ..a
        }
        .can_sell(100));
        assert!(Availability {
            tracked: false,
            ..a
        }
        .can_sell(100));
        assert_eq!(Availability { reserved: 9, ..a }.available(), 0);
    }
}
