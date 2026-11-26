//! Inventory.
//!
//! A shop keeps stock at [`locations`]. Each stock item (a variant, or a
//! product without variants, see [`soat_catalog::stock_item`]) has units on
//! hand per location; every change goes through an adjustment with a
//! reason and lands in a ledger ([`stock`]). Carts and orders hold units
//! with [`reservations`], which subtract from what is available.
//!
//! When a product or variant is deleted, the `inventory` subscriber drops
//! its stock (the ledger stays).

pub mod locations;
pub mod reservations;
pub mod stock;

use std::sync::Arc;

use soat_core::Permission;
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use zapline::db::sqlx::{self, PgPool};
use zapline::embed::Dir;
use zapline::prelude::App;
use zapline::Resources;

/// SQL: whether item `$2` of shop `$1` is tracked when staff never said.
/// Products that ship are; digital products are not, so they sell
/// without stock unless staff track them (limited licenses, say).
pub(crate) const TRACKED: &str = "COALESCE( \
     (SELECT tracked FROM inventory_items WHERE tenant_id = $1 AND item_id = $2), \
     (SELECT p.requires_shipping FROM products p WHERE p.tenant_id = $1 AND p.id = COALESCE( \
         (SELECT v.product_id FROM product_variants v WHERE v.tenant_id = $1 AND v.id = $2), $2)), \
     TRUE)";

/// See stock and locations.
pub const INVENTORY_READ: Permission = Permission::new(
    "inventory:read",
    "See stock levels, locations and the ledger",
)
.for_staff();
/// Adjust stock and its settings.
pub const INVENTORY_WRITE: Permission =
    Permission::new("inventory:write", "Adjust stock and its settings").for_staff();
/// Create, change and delete locations.
pub const LOCATIONS_MANAGE: Permission = Permission::new(
    "locations:manage",
    "Create, change and delete stock locations",
);

/// The inventory module.
pub struct Inventory;

impl Module for Inventory {
    fn name(&self) -> &'static str {
        "inventory"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[INVENTORY_READ, INVENTORY_WRITE, LOCATIONS_MANAGE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Cleanup)]
    }

    fn mount(&self, app: App) -> App {
        // Reservations first: `/api/inventory/reservations` must not be
        // taken for an item id.
        app.plugin(locations::mount)
            .plugin(reservations::mount)
            .plugin(stock::mount)
    }
}

/// Drops the stock of deleted products and variants. Idempotent.
struct Cleanup;

impl Subscriber for Cleanup {
    fn name(&self) -> &'static str {
        "inventory"
    }

    fn wants(&self, kind: &str) -> bool {
        matches!(kind, "product.deleted" | "variant.deleted")
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let mut items: Vec<String> = Vec::new();
            if let Some(id) = event.payload["id"].as_str() {
                items.push(id.to_owned());
            }
            if let Some(ids) = event.payload["variant_ids"].as_array() {
                items.extend(ids.iter().filter_map(|v| v.as_str().map(str::to_owned)));
            }
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            for table in ["inventory_levels", "inventory_items"] {
                sqlx::query(&format!(
                    "DELETE FROM {table} WHERE tenant_id = $1 AND item_id = ANY($2)"
                ))
                .bind(&event.tenant_id)
                .bind(&items)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
    }
}
