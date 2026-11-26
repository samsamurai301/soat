//! Bundles: a product sold as a set of other stock items (two tees and a
//! cap, a starter kit). The bundle has its own price; its stock is what
//! its items' stock allows, and selling it holds and ships them.

use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{stock_item, PRODUCTS_READ, PRODUCTS_WRITE};

/// Most items in one bundle.
pub const MAX_ITEMS: usize = 50;

/// Units of a stock item in a bundle.
#[model]
#[derive(sqlx::FromRow)]
pub struct BundleItem {
    /// A variant, or a product without variants; never another bundle.
    pub item_id: String,
    #[validate(range(min = 1, max = 1000))]
    pub quantity: i32,
}

/// Body of `PUT /api/products/{id}/bundle`.
#[model]
pub struct BundleItems {
    /// In display order; empty makes the product an ordinary one again.
    pub items: Vec<BundleItem>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_bundle, set_bundle))
}

/// What bundle `item_id` is made of, as `(item id, units)`; empty when it
/// is no bundle. Run it in the tenant transaction.
pub async fn bundle_items(
    conn: &mut PgConnection,
    scope: &impl Scope,
    item_id: &str,
) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT item_id, quantity FROM product_bundle_items \
         WHERE tenant_id = $1 AND bundle_id = $2 ORDER BY position",
    )
    .bind(scope.tenant_id())
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(i, q)| (i, i64::from(q))).collect())
}

async fn items(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Vec<BundleItem>> {
    Ok(sqlx::query_as(
        "SELECT item_id, quantity FROM product_bundle_items \
         WHERE tenant_id = $1 AND bundle_id = $2 ORDER BY position",
    )
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The items a bundle is made of (`products:read`); empty for a product
/// that is no bundle.
#[get("/api/products/{id}/bundle", tag = "catalog")]
async fn get_bundle(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<BundleItems>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    let items = items(&mut tx, &tenant, &product).await?;
    tx.commit().await?;
    Ok(Json(BundleItems { items }))
}

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_bundle")
}

/// Make a product (without variants) a bundle of other items, or an
/// ordinary product again with no items (`products:write`). Items are
/// variants or products without variants, each once, never a bundle or
/// the product itself: else a 422 `invalid_bundle`. Publishes
/// `product.bundle_changed`.
#[put("/api/products/{id}/bundle", tag = "catalog")]
async fn set_bundle(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(body): Json<BundleItems>,
) -> Result<Json<BundleItems>> {
    if body.items.len() > MAX_ITEMS {
        return Err(invalid(format!("at most {MAX_ITEMS} items")));
    }
    if body.items.iter().any(|i| !(1..=1000).contains(&i.quantity)) {
        return Err(invalid("each item is 1 to 1000 units"));
    }
    let mut seen: Vec<&str> = body.items.iter().map(|i| i.item_id.as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    if seen.len() != body.items.len() {
        return Err(invalid("list each item once"));
    }
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    if !body.items.is_empty() {
        // A product with variants is no stock item, so cannot be a bundle.
        stock_item(&mut tx, &tenant, &product).await?;
        let used: Option<(String,)> = sqlx::query_as(
            "SELECT bundle_id FROM product_bundle_items WHERE tenant_id = $1 AND item_id = $2 \
             LIMIT 1",
        )
        .bind(tenant.id())
        .bind(&product)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((b,)) = used {
            return Err(invalid(format!("it is an item of bundle {b}")));
        }
    }
    for i in &body.items {
        if i.item_id == product {
            return Err(invalid("a bundle cannot hold itself"));
        }
        let item = stock_item(&mut tx, &tenant, &i.item_id)
            .await
            .map_err(|_| invalid(format!("{} has variants: bundle one", i.item_id)))?
            .ok_or_else(|| invalid(format!("no item {}", i.item_id)))?;
        if !bundle_items(&mut tx, &tenant, &item.id).await?.is_empty() {
            return Err(invalid(format!("{} is a bundle itself", item.sku)));
        }
    }
    sqlx::query("DELETE FROM product_bundle_items WHERE tenant_id = $1 AND bundle_id = $2")
        .bind(tenant.id())
        .bind(&product)
        .execute(&mut *tx)
        .await?;
    for (position, i) in body.items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO product_bundle_items (tenant_id, bundle_id, item_id, quantity, position) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant.id())
        .bind(&product)
        .bind(&i.item_id)
        .bind(i.quantity)
        .bind(position as i32)
        .execute(&mut *tx)
        .await?;
    }
    tenant
        .publish(
            &mut tx,
            "product.bundle_changed",
            &serde_json::json!({ "product_id": product, "items": body.items }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(body))
}
