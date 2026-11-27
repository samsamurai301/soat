//! Wishlists.
//!
//! A signed-in customer saves products for later and removes them again;
//! their list shows each product as it is today (products no longer for
//! sale drop out of it). Staff see which products are saved most.

use serde_json::json;
use soat_core::Permission;
use soat_customers::accounts::SignedIn;
use soat_module::Module;
use soat_tenancy::{Scope, Shop, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

/// See which products customers saved.
pub const WISHLISTS_READ: Permission =
    Permission::new("wishlists:read", "See which products customers saved").for_staff();

/// Most products one wishlist holds.
pub const MAX_ITEMS: i64 = 200;

/// The wishlists module.
pub struct Wishlists;

impl Module for Wishlists {
    fn name(&self) -> &'static str {
        "wishlists"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog", "customers"]
    }

    fn migrations(&self) -> Option<zapline::embed::Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[WISHLISTS_READ]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[soat_module::PersonalData {
            table: "wishlist_items",
            matches: "customer_id = $2",
            export: &["product_id", "created_at"],
            erase: soat_module::Erase::Delete,
        }]
    }

    fn mount(&self, app: App) -> App {
        app.mount((my_wishlist, save_item, remove_item, most_saved))
    }
}

/// A saved product as it is today.
#[model]
#[derive(sqlx::FromRow)]
pub struct WishlistItem {
    pub product_id: String,
    pub handle: String,
    pub title: String,
    pub price_minor: i64,
    pub currency: String,
    /// When it was saved, RFC 3339.
    pub saved_at: String,
}

/// A product and how many customers saved it.
#[model]
#[derive(sqlx::FromRow)]
pub struct SavedProduct {
    pub product_id: String,
    pub title: String,
    pub saves: i64,
}

/// Customer `customer`'s saved products that are for sale, newest first.
///
/// # Errors
///
/// Database errors.
pub async fn items(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
) -> Result<Vec<WishlistItem>> {
    Ok(sqlx::query_as(
        "SELECT w.product_id, p.handle, p.title, p.price_minor, p.currency, \
         to_char(w.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS saved_at \
         FROM wishlist_items w JOIN products p \
         ON p.tenant_id = w.tenant_id AND p.id = w.product_id AND p.status = 'active' \
         WHERE w.tenant_id = $1 AND w.customer_id = $2 \
         ORDER BY w.created_at DESC, w.product_id",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .fetch_all(conn)
    .await?)
}

/// Whether customer `customer` saved `product`.
///
/// # Errors
///
/// Database errors.
pub async fn saved(
    conn: &mut PgConnection,
    scope: &impl Scope,
    customer: &str,
    product: &str,
) -> Result<bool> {
    let (yes,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM wishlist_items WHERE tenant_id = $1 \
         AND customer_id = $2 AND product_id = $3)",
    )
    .bind(scope.tenant_id())
    .bind(customer)
    .bind(product)
    .fetch_one(conn)
    .await?;
    Ok(yes)
}

/// Saves `product` for customer `customer`; saving twice changes nothing.
/// Publishes `wishlist.item_added` the first time.
///
/// # Errors
///
/// 404 for a product the shop does not sell; 409 `wishlist_full` past
/// [`MAX_ITEMS`]; database errors.
pub async fn add(tx: &mut TenantTx, shop: &Shop, customer: &str, product: &str) -> Result<()> {
    let (active,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM products WHERE tenant_id = $1 AND id = $2 \
         AND status = 'active')",
    )
    .bind(shop.id())
    .bind(product)
    .fetch_one(&mut **tx)
    .await?;
    if !active {
        return Err(Error::not_found());
    }
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM wishlist_items WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(shop.id())
    .bind(customer)
    .fetch_one(&mut **tx)
    .await?;
    let added = sqlx::query(
        "INSERT INTO wishlist_items (tenant_id, customer_id, product_id) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(shop.id())
    .bind(customer)
    .bind(product)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if added == 0 {
        return Ok(());
    }
    if count >= MAX_ITEMS {
        return Err(
            Error::conflict(format!("a wishlist holds up to {MAX_ITEMS} products"))
                .with_code("wishlist_full"),
        );
    }
    shop.publish(
        tx,
        None,
        "wishlist.item_added",
        &json!({ "customer_id": customer, "product_id": product }),
    )
    .await?;
    Ok(())
}

/// Removes `product` from customer `customer`'s list, if it is there.
/// Publishes `wishlist.item_removed` when it was.
///
/// # Errors
///
/// Database errors.
pub async fn remove(tx: &mut TenantTx, shop: &Shop, customer: &str, product: &str) -> Result<()> {
    let gone = sqlx::query(
        "DELETE FROM wishlist_items WHERE tenant_id = $1 AND customer_id = $2 AND product_id = $3",
    )
    .bind(shop.id())
    .bind(customer)
    .bind(product)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if gone > 0 {
        shop.publish(
            tx,
            None,
            "wishlist.item_removed",
            &json!({ "customer_id": customer, "product_id": product }),
        )
        .await?;
    }
    Ok(())
}

/// The signed-in customer's saved products that are still for sale,
/// newest first.
#[get("/api/storefront/account/wishlist", tag = "storefront")]
async fn my_wishlist(db: Db, signed_in: SignedIn) -> Result<Json<Vec<WishlistItem>>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let list = items(&mut tx, &signed_in.shop, &customer).await?;
    tx.commit().await?;
    Ok(Json(list))
}

/// Save a product to the signed-in customer's wishlist; saving it again
/// changes nothing. Publishes `wishlist.item_added`.
#[put(
    "/api/storefront/account/wishlist/{product_id}",
    tag = "storefront",
    status = 204
)]
async fn save_item(db: Db, signed_in: SignedIn, Path(product): Path<String>) -> Result<StatusCode> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    add(&mut tx, &signed_in.shop, &customer, &product).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a product from the signed-in customer's wishlist. Publishes
/// `wishlist.item_removed` when it was there.
#[delete(
    "/api/storefront/account/wishlist/{product_id}",
    tag = "storefront",
    status = 204
)]
async fn remove_item(
    db: Db,
    signed_in: SignedIn,
    Path(product): Path<String>,
) -> Result<StatusCode> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    remove(&mut tx, &signed_in.shop, &customer, &product).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The products customers saved most, up to 50 (`wishlists:read`).
#[get("/api/wishlists/most-saved", tag = "wishlists")]
async fn most_saved(db: Db, tenant: Tenant) -> Result<Json<Vec<SavedProduct>>> {
    let mut tx = tenant.begin_for(&db, &WISHLISTS_READ).await?;
    let rows: Vec<SavedProduct> = sqlx::query_as(
        "SELECT w.product_id, COALESCE(p.title, '') AS title, COUNT(*) AS saves \
         FROM wishlist_items w LEFT JOIN products p \
         ON p.tenant_id = w.tenant_id AND p.id = w.product_id \
         WHERE w.tenant_id = $1 GROUP BY w.product_id, p.title \
         ORDER BY saves DESC, w.product_id LIMIT 50",
    )
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(rows))
}
