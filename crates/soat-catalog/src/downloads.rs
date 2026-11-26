//! A digital product's downloads: files the buyer gets once their order
//! is paid. Upload with `POST /api/files`, then list the file ids here.

use soat_files::FileView;
use soat_tenancy::Tenant;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::{PRODUCTS_READ, PRODUCTS_WRITE};

/// Most downloads on one product.
pub const MAX_DOWNLOADS: usize = 50;

/// Body of `PUT /api/products/{id}/downloads`.
#[model]
pub struct ProductDownloads {
    /// File ids in the order the buyer sees them.
    pub file_ids: Vec<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_downloads, set_downloads))
}

async fn download_ids(
    tx: &mut soat_tenancy::TenantTx,
    tenant: &Tenant,
    product: &str,
) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT file_id FROM product_downloads WHERE tenant_id = $1 AND product_id = $2 \
         ORDER BY position",
    )
    .bind(tenant.id())
    .bind(product)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// The product's downloads in order, with links (`products:read`).
#[get("/api/products/{id}/downloads", tag = "catalog")]
async fn get_downloads(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<FileView>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    let ids = download_ids(&mut tx, &tenant, &product).await?;
    let views = soat_files::views(&mut tx, &tenant, &storage, &ids).await?;
    tx.commit().await?;
    Ok(Json(views))
}

/// Set the product's downloads, in order (`products:write`). Each id must
/// be one of the shop's files, listed once; at most 50. Publishes
/// `product.downloads_changed`.
#[put("/api/products/{id}/downloads", tag = "catalog")]
async fn set_downloads(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
    Json(body): Json<ProductDownloads>,
) -> Result<Json<Vec<FileView>>> {
    let ids = body.file_ids;
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != ids.len() || ids.len() > MAX_DOWNLOADS {
        return Err(
            Error::unprocessable(format!("list each file once, at most {MAX_DOWNLOADS}"))
                .with_code("invalid_downloads"),
        );
    }
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    if soat_files::count_owned(&mut tx, &tenant, &ids).await? != ids.len() as i64 {
        return Err(Error::unprocessable("unknown file id").with_code("unknown_file"));
    }
    sqlx::query("DELETE FROM product_downloads WHERE tenant_id = $1 AND product_id = $2")
        .bind(tenant.id())
        .bind(&product)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO product_downloads (tenant_id, product_id, file_id, position) \
         SELECT $1, $2, f.id, f.n - 1 FROM unnest($3::text[]) WITH ORDINALITY AS f(id, n)",
    )
    .bind(tenant.id())
    .bind(&product)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "product.downloads_changed",
            &serde_json::json!({ "product_id": product, "file_ids": ids }),
        )
        .await?;
    let views = soat_files::views(&mut tx, &tenant, &storage, &ids).await?;
    tx.commit().await?;
    Ok(Json(views))
}

/// The downloads of `product_ids`, as `(product id, file id)` in each
/// product's order. Run it in the tenant transaction.
pub async fn downloads_of(
    conn: &mut sqlx::PgConnection,
    scope: &impl soat_tenancy::Scope,
    product_ids: &[String],
) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT product_id, file_id FROM product_downloads \
         WHERE tenant_id = $1 AND product_id = ANY($2) ORDER BY product_id, position",
    )
    .bind(scope.tenant_id())
    .bind(product_ids)
    .fetch_all(&mut *conn)
    .await?)
}
