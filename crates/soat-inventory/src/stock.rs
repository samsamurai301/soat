//! Stock levels and the ledger of every change.
//!
//! Units on hand never go below zero. Each change is an adjustment with a
//! reason; a count sets the level and records the difference; a transfer
//! moves units between two locations. Concurrent changes to one level
//! queue on its row lock, so none is lost.

use soat_catalog::{stock_item, StockItem};
use soat_core::id_kind;
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::locations::load as load_location;
use crate::{INVENTORY_READ, INVENTORY_WRITE};

id_kind!(AdjustmentId, AdjustmentKind, "adj");

/// Reasons a person may give for an adjustment. `count` and `transfer` are
/// set by their own endpoints; orders will add `sold` and `restocked`.
pub const REASONS: &[&str] = &[
    "received",
    "correction",
    "damaged",
    "returned",
    "lost",
    "other",
];

/// An item's stock everywhere.
#[model]
pub struct InventoryView {
    /// `var_...`, or `prod_...` for a product without variants.
    pub item_id: String,
    pub sku: String,
    /// Untracked items are always available. Digital products are
    /// untracked until staff track them.
    pub tracked: bool,
    /// Whether it may be sold when nothing is on hand.
    pub allow_backorder: bool,
    /// Units on hand at all locations.
    pub on_hand: i64,
    /// Units held by carts and orders.
    pub reserved: i64,
    /// On hand minus reserved, never below zero.
    pub available: i64,
    pub levels: Vec<Level>,
}

/// Units at one location.
#[model]
#[derive(sqlx::FromRow)]
pub struct Level {
    pub item_id: String,
    pub location_id: String,
    pub on_hand: i64,
}

/// Body of an adjustment.
#[model]
pub struct Adjust {
    pub location_id: String,
    /// Units to add (positive) or remove (negative); never 0.
    pub delta: i64,
    /// One of `received`, `correction`, `damaged`, `returned`, `lost`,
    /// `other`.
    pub reason: String,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of a stock count.
#[model]
pub struct Count {
    pub location_id: String,
    #[validate(range(min = 0))]
    pub on_hand: i64,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of a transfer.
#[model]
pub struct Transfer {
    pub from_location_id: String,
    pub to_location_id: String,
    #[validate(range(min = 1))]
    pub quantity: i64,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

/// Body of a settings change.
#[model]
pub struct StockSettings {
    pub tracked: Option<bool>,
    pub allow_backorder: Option<bool>,
}

/// One line of the ledger.
#[model]
#[derive(sqlx::FromRow)]
pub struct Adjustment {
    /// `adj_...`, time-ordered.
    pub id: String,
    pub item_id: String,
    pub location_id: String,
    pub delta: i64,
    pub on_hand_after: i64,
    pub reason: String,
    pub note: String,
    /// `user_...` or `key_...`.
    pub actor: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_levels,
        get_inventory,
        update_settings,
        adjust,
        count,
        transfer,
        ledger,
    ))
}

async fn item(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<StockItem> {
    stock_item(tx, tenant, id)
        .await?
        .ok_or_else(Error::not_found)
}

async fn view(tx: &mut TenantTx, tenant: &Tenant, item: StockItem) -> Result<InventoryView> {
    let settings: Option<(bool, bool)> = sqlx::query_as(
        "SELECT tracked, allow_backorder FROM inventory_items WHERE tenant_id = $1 AND item_id = $2",
    )
    .bind(tenant.id())
    .bind(&item.id)
    .fetch_optional(&mut **tx)
    .await?;
    let (tracked, allow_backorder) = settings.unwrap_or((item.requires_shipping, false));
    let levels: Vec<Level> = sqlx::query_as(
        "SELECT item_id, location_id, on_hand FROM inventory_levels \
         WHERE tenant_id = $1 AND item_id = $2 ORDER BY location_id",
    )
    .bind(tenant.id())
    .bind(&item.id)
    .fetch_all(&mut **tx)
    .await?;
    let now = crate::reservations::availability(tx, tenant, &item.id).await?;
    Ok(InventoryView {
        on_hand: now.on_hand,
        reserved: now.reserved,
        available: now.available(),
        item_id: item.id,
        sku: item.sku,
        tracked,
        allow_backorder,
        levels,
    })
}

/// Applies `delta` at one location under a row lock and records it in the
/// ledger with `reason`. Returns the new level, or 409
/// `insufficient_stock`. Publishes `inventory.adjusted`.
pub async fn apply(
    tx: &mut TenantTx,
    tenant: &impl Scope,
    item: &str,
    location: &str,
    delta: i64,
    reason: &str,
    note: &str,
) -> Result<i64> {
    sqlx::query(
        "INSERT INTO inventory_levels (tenant_id, item_id, location_id, on_hand) \
         VALUES ($1, $2, $3, 0) ON CONFLICT DO NOTHING",
    )
    .bind(tenant.tenant_id())
    .bind(item)
    .bind(location)
    .execute(&mut **tx)
    .await?;
    let (current,): (i64,) = sqlx::query_as(
        "SELECT on_hand FROM inventory_levels \
         WHERE tenant_id = $1 AND item_id = $2 AND location_id = $3 FOR UPDATE",
    )
    .bind(tenant.tenant_id())
    .bind(item)
    .bind(location)
    .fetch_one(&mut **tx)
    .await?;
    let after = current
        .checked_add(delta)
        .filter(|n| *n >= 0)
        .ok_or_else(|| {
            Error::conflict(format!("only {current} on hand at this location"))
                .with_code("insufficient_stock")
        })?;
    sqlx::query(
        "UPDATE inventory_levels SET on_hand = $4 \
         WHERE tenant_id = $1 AND item_id = $2 AND location_id = $3",
    )
    .bind(tenant.tenant_id())
    .bind(item)
    .bind(location)
    .bind(after)
    .execute(&mut **tx)
    .await?;
    let adjustment = Adjustment {
        id: AdjustmentId::new().to_string(),
        item_id: item.to_owned(),
        location_id: location.to_owned(),
        delta,
        on_hand_after: after,
        reason: reason.to_owned(),
        note: note.to_owned(),
        actor: tenant.actor_id().map(str::to_owned),
    };
    sqlx::query(
        "INSERT INTO inventory_adjustments \
         (id, tenant_id, item_id, location_id, delta, on_hand_after, reason, note, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&adjustment.id)
    .bind(tenant.tenant_id())
    .bind(item)
    .bind(location)
    .bind(delta)
    .bind(after)
    .bind(reason)
    .bind(note)
    .bind(&adjustment.actor)
    .execute(&mut **tx)
    .await?;
    tenant
        .publish(tx, "inventory.adjusted", &adjustment)
        .await?;
    Ok(after)
}

/// Takes `quantity` of a shipped item out of stock for `reference` (an
/// order): from `location`, or else the location with the most on hand,
/// recorded as `sold`, and lets go of as much of the reference's
/// reservation. Untracked items only lose the reservation. A bundle ships
/// its items. Returns the location used, if any (for a bundle, its first
/// item's).
pub async fn ship(
    tx: &mut TenantTx,
    scope: &impl Scope,
    item: &str,
    quantity: i64,
    location: Option<&str>,
    reference: &str,
) -> Result<Option<String>> {
    let parts = soat_catalog::bundles::bundle_items(tx, scope, item).await?;
    if parts.is_empty() {
        return ship_one(tx, scope, item, quantity, location, reference).await;
    }
    let mut first = None;
    for (part, per) in &parts {
        let used = ship_one(tx, scope, part, quantity * per, location, reference).await?;
        first = first.or(used);
    }
    Ok(first)
}

async fn ship_one(
    tx: &mut TenantTx,
    scope: &impl Scope,
    item: &str,
    quantity: i64,
    location: Option<&str>,
    reference: &str,
) -> Result<Option<String>> {
    let (tracked,): (bool,) = sqlx::query_as(&format!("SELECT {}", crate::TRACKED))
        .bind(scope.tenant_id())
        .bind(item)
        .fetch_one(&mut **tx)
        .await?;
    let used = if tracked {
        let from = match location {
            Some(l) => l.to_owned(),
            None => {
                let best: Option<(String,)> = sqlx::query_as(
                    "SELECT location_id FROM inventory_levels \
                     WHERE tenant_id = $1 AND item_id = $2 ORDER BY on_hand DESC, location_id \
                     LIMIT 1",
                )
                .bind(scope.tenant_id())
                .bind(item)
                .fetch_optional(&mut **tx)
                .await?;
                best.map(|(l,)| l).ok_or_else(|| {
                    Error::conflict(format!("{item} is not stocked anywhere"))
                        .with_code("insufficient_stock")
                })?
            }
        };
        apply(tx, scope, item, &from, -quantity, "sold", reference).await?;
        Some(from)
    } else {
        None
    };
    crate::reservations::take(tx, scope, reference, item, quantity).await?;
    Ok(used)
}

/// Puts `quantity` of an item back at `location` for `reference`, recorded
/// as `restocked`; a bundle puts back its items. Untracked items are left
/// alone.
pub async fn restock(
    tx: &mut TenantTx,
    scope: &impl Scope,
    item: &str,
    quantity: i64,
    location: &str,
    reference: &str,
) -> Result<()> {
    let parts = soat_catalog::bundles::bundle_items(tx, scope, item).await?;
    if parts.is_empty() {
        apply(tx, scope, item, location, quantity, "restocked", reference).await?;
    }
    for (part, per) in &parts {
        let (tracked,): (bool,) = sqlx::query_as(&format!("SELECT {}", crate::TRACKED))
            .bind(scope.tenant_id())
            .bind(part)
            .fetch_one(&mut **tx)
            .await?;
        if tracked {
            apply(
                tx,
                scope,
                part,
                location,
                quantity * per,
                "restocked",
                reference,
            )
            .await?;
        }
    }
    Ok(())
}

/// Stock levels of the shop, 50 per page (`inventory:read`). Filter with
/// `?location_id=` or `?item_id=`; `?sort=on_hand` lists the lowest first.
#[get("/api/inventory", tag = "inventory")]
async fn list_levels(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Level>>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let page = TenantListing::new(
        "(SELECT item_id || '@' || location_id AS id, item_id, location_id, on_hand \
          FROM inventory_levels) AS levels",
        "item_id, location_id, on_hand",
    )
    .sortable(["on_hand", "item_id", "location_id"])
    .filter_text("location_id")
    .filter_text("item_id")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// An item's stock at every location (`inventory:read`). The item is a
/// variant id, or the id of a product without variants.
#[get("/api/inventory/{item_id}", tag = "inventory")]
async fn get_inventory(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
) -> Result<Json<InventoryView>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let item = item(&mut tx, &tenant, &item_id).await?;
    let view = view(&mut tx, &tenant, item).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Track an item or not, and allow backorders (`inventory:write`).
/// Publishes `inventory.settings_changed`.
#[patch("/api/inventory/{item_id}", tag = "inventory")]
async fn update_settings(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    Json(s): Json<StockSettings>,
) -> Result<Json<InventoryView>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let item = item(&mut tx, &tenant, &item_id).await?;
    sqlx::query(
        "INSERT INTO inventory_items (tenant_id, item_id, tracked, allow_backorder) \
         VALUES ($1, $2, COALESCE($3, $5), COALESCE($4, FALSE)) \
         ON CONFLICT (tenant_id, item_id) DO UPDATE SET \
         tracked = COALESCE($3, inventory_items.tracked), \
         allow_backorder = COALESCE($4, inventory_items.allow_backorder)",
    )
    .bind(tenant.id())
    .bind(&item.id)
    .bind(s.tracked)
    .bind(s.allow_backorder)
    .bind(item.requires_shipping)
    .execute(&mut *tx)
    .await?;
    let view = view(&mut tx, &tenant, item).await?;
    tenant
        .publish(&mut tx, "inventory.settings_changed", &view)
        .await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Add or remove units at a location with a reason (`inventory:write`).
/// Going below zero is a 409 `insufficient_stock`. Publishes
/// `inventory.adjusted`.
#[post("/api/inventory/{item_id}/adjustments", tag = "inventory")]
async fn adjust(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    Valid(Json(a)): Valid<Json<Adjust>>,
) -> Result<Json<InventoryView>> {
    if a.delta == 0 {
        return Err(Error::unprocessable("delta must not be 0").with_code("invalid_delta"));
    }
    if !REASONS.contains(&a.reason.as_str()) {
        return Err(
            Error::unprocessable(format!("reason is one of: {}", REASONS.join(", ")))
                .with_code("invalid_reason"),
        );
    }
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let item = item(&mut tx, &tenant, &item_id).await?;
    let location = location(&mut tx, &tenant, &a.location_id).await?;
    let note = a.note.unwrap_or_default();
    apply(
        &mut tx, &tenant, &item.id, &location, a.delta, &a.reason, &note,
    )
    .await?;
    let view = view(&mut tx, &tenant, item).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Record a stock count: set the units at a location; the difference goes
/// to the ledger as `count` (`inventory:write`).
#[post("/api/inventory/{item_id}/count", tag = "inventory")]
async fn count(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    Valid(Json(c)): Valid<Json<Count>>,
) -> Result<Json<InventoryView>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let item = item(&mut tx, &tenant, &item_id).await?;
    let location = location(&mut tx, &tenant, &c.location_id).await?;
    let (current,): (Option<i64>,) = sqlx::query_as(
        "SELECT on_hand FROM inventory_levels \
         WHERE tenant_id = $1 AND item_id = $2 AND location_id = $3 FOR UPDATE",
    )
    .bind(tenant.id())
    .bind(&item.id)
    .bind(&location)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or((None,));
    let delta = c.on_hand - current.unwrap_or(0);
    if delta != 0 {
        let note = c.note.unwrap_or_default();
        apply(&mut tx, &tenant, &item.id, &location, delta, "count", &note).await?;
    }
    let view = view(&mut tx, &tenant, item).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Move units from one location to another (`inventory:write`). Both
/// sides land in the ledger as `transfer`.
#[post("/api/inventory/{item_id}/transfers", tag = "inventory")]
async fn transfer(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    Valid(Json(t)): Valid<Json<Transfer>>,
) -> Result<Json<InventoryView>> {
    if t.from_location_id == t.to_location_id {
        return Err(Error::unprocessable("pick two different locations").with_code("same_location"));
    }
    let mut tx = tenant.begin_for(&db, &INVENTORY_WRITE).await?;
    let item = item(&mut tx, &tenant, &item_id).await?;
    let from = location(&mut tx, &tenant, &t.from_location_id).await?;
    let to = location(&mut tx, &tenant, &t.to_location_id).await?;
    let note = t.note.unwrap_or_default();
    apply(
        &mut tx,
        &tenant,
        &item.id,
        &from,
        -t.quantity,
        "transfer",
        &note,
    )
    .await?;
    apply(
        &mut tx, &tenant, &item.id, &to, t.quantity, "transfer", &note,
    )
    .await?;
    let view = view(&mut tx, &tenant, item).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Every change of an item's stock, newest first (`inventory:read`).
/// `?location_id=` and `?reason=` filter.
#[get("/api/inventory/{item_id}/adjustments", tag = "inventory")]
async fn ledger(
    db: Db,
    tenant: Tenant,
    Path(item_id): Path<String>,
    params: ListParams,
) -> Result<Json<Paged<Adjustment>>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let page = TenantListing::new(
        "inventory_adjustments",
        "id, item_id, location_id, delta, on_hand_after, reason, note, actor",
    )
    .scope_text("item_id", item_id)
    .default_sort("-id")
    .filter_text("location_id")
    .filter_text("reason")
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// A location named in a body: unknown is a 422, since the path was found.
async fn location(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<String> {
    load_location(tx, tenant, id)
        .await
        .map(|l| l.id)
        .map_err(|_| {
            Error::unprocessable(format!("no location {id}")).with_code("unknown_location")
        })
}
