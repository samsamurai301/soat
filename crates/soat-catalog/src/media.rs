//! A product's images and videos, in order. Upload with `POST /api/files`,
//! then list the file ids here; the first is the main image.

use soat_files::FileView;
use soat_tenancy::Tenant;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::{PRODUCTS_READ, PRODUCTS_WRITE};

/// Most media on one product.
pub const MAX_MEDIA: usize = 250;

/// Body of `PUT /api/products/{id}/media`.
#[model]
pub struct ProductMedia {
    /// File ids in display order.
    pub file_ids: Vec<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_media, set_media, import_media))
}

/// Most links in one `POST /api/products/{id}/media/import`.
pub const MAX_IMPORT: usize = 20;

/// Body of `POST /api/products/{id}/media/import`.
#[model]
pub struct ImportMedia {
    /// Links to images, in order; at most 20.
    pub urls: Vec<String>,
}

/// A link that could not be imported.
#[model]
pub struct MediaImportProblem {
    pub url: String,
    pub message: String,
}

/// What `POST /api/products/{id}/media/import` did.
#[model]
pub struct MediaImported {
    /// How many links became new media.
    pub imported: usize,
    /// Links imported before, skipped.
    pub skipped: usize,
    pub problems: Vec<MediaImportProblem>,
    /// The product's media now, in order.
    pub media: Vec<FileView>,
}

async fn media_ids(
    tx: &mut soat_tenancy::TenantTx,
    tenant: &Tenant,
    product: &str,
) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT file_id FROM product_media WHERE tenant_id = $1 AND product_id = $2 \
         ORDER BY position",
    )
    .bind(tenant.id())
    .bind(product)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// The product's media in order, with links (`products:read`).
#[get("/api/products/{id}/media", tag = "catalog")]
async fn get_media(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<FileView>>> {
    let mut tx = tenant.begin_for(&db, &PRODUCTS_READ).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    let ids = media_ids(&mut tx, &tenant, &product).await?;
    let views = soat_files::views(&mut tx, &tenant, &storage, &ids).await?;
    tx.commit().await?;
    Ok(Json(views))
}

/// Set the product's media, in order (`products:write`). Each id must be
/// one of the shop's files, listed once; at most 250. Publishes
/// `product.media_changed`.
#[put("/api/products/{id}/media", tag = "catalog")]
async fn set_media(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
    Json(body): Json<ProductMedia>,
) -> Result<Json<Vec<FileView>>> {
    let ids = body.file_ids;
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != ids.len() || ids.len() > MAX_MEDIA {
        return Err(
            Error::unprocessable(format!("list each file once, at most {MAX_MEDIA}"))
                .with_code("invalid_media"),
        );
    }
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    if soat_files::count_owned(&mut tx, &tenant, &ids).await? != ids.len() as i64 {
        return Err(Error::unprocessable("unknown file id").with_code("unknown_file"));
    }
    sqlx::query("DELETE FROM product_media WHERE tenant_id = $1 AND product_id = $2")
        .bind(tenant.id())
        .bind(&product)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO product_media (tenant_id, product_id, file_id, position) \
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
            "product.media_changed",
            &serde_json::json!({ "product_id": product, "file_ids": ids }),
        )
        .await?;
    let views = soat_files::views(&mut tx, &tenant, &storage, &ids).await?;
    tx.commit().await?;
    Ok(Json(views))
}

/// Add images to the product from links (`products:write`), such as an
/// import file's: each is fetched (`https://` to public addresses, up to
/// 20 MB, an image or video SOAT stores) and added after its media, in
/// order. A link imported for the product before is skipped, also when
/// staff removed its image since. A link that fails is reported in
/// `problems` and the others still go in. Publishes
/// `product.media_changed` when any was added.
#[post("/api/products/{id}/media/import", tag = "catalog")]
async fn import_media(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    // The webhooks module's: the same links a shop may have SOAT call.
    policy: State<soat_http::TargetPolicy>,
    Path(id): Path<String>,
    Json(body): Json<ImportMedia>,
) -> Result<Json<MediaImported>> {
    if body.urls.len() > MAX_IMPORT {
        return Err(
            Error::unprocessable(format!("at most {MAX_IMPORT} links at a time"))
                .with_code("invalid_media"),
        );
    }
    let urls = soat_core::import::image_urls(&body.urls.join(" "));
    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = crate::existing(&mut tx, &tenant, &id).await?;
    let known: Vec<(String,)> = sqlx::query_as(
        "SELECT url FROM product_media_sources WHERE tenant_id = $1 AND product_id = $2 \
         AND url = ANY($3)",
    )
    .bind(tenant.id())
    .bind(&product)
    .bind(&urls)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let fresh: Vec<String> = urls
        .iter()
        .filter(|u| !known.iter().any(|(k,)| k == *u))
        .cloned()
        .collect();
    let skipped = urls.len() - fresh.len();

    // Fetch without holding a transaction.
    let mut fetched = Vec::new();
    let mut problems = Vec::new();
    for url in fresh {
        match soat_http::fetch(&url, policy.0, soat_files::MAX_UPLOAD).await {
            Ok(f) => fetched.push((url, f.body)),
            Err(e) => problems.push(MediaImportProblem {
                message: e.to_string(),
                url,
            }),
        }
    }

    let mut tx = tenant.begin_for(&db, &PRODUCTS_WRITE).await?;
    let product = crate::existing(&mut tx, &tenant, &product).await?;
    let mut ids = media_ids(&mut tx, &tenant, &product).await?;
    let mut imported = 0;
    for (url, bytes) in fetched {
        if ids.len() >= MAX_MEDIA {
            problems.push(MediaImportProblem {
                url,
                message: format!("the product has {MAX_MEDIA} media already"),
            });
            continue;
        }
        let name = url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or("image")
            .to_owned();
        let file = match soat_files::store(&mut tx, &tenant, &storage, &name, bytes).await {
            Ok(file) => file,
            Err(e) => {
                problems.push(MediaImportProblem {
                    url,
                    message: e.to_string(),
                });
                continue;
            }
        };
        let added = sqlx::query(
            "INSERT INTO product_media_sources (tenant_id, product_id, url, file_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(tenant.id())
        .bind(&product)
        .bind(&url)
        .bind(&file.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if added == 0 {
            // Another import took this link meanwhile.
            continue;
        }
        sqlx::query(
            "INSERT INTO product_media (tenant_id, product_id, file_id, position) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(tenant.id())
        .bind(&product)
        .bind(&file.id)
        .bind(ids.len() as i32)
        .execute(&mut *tx)
        .await?;
        ids.push(file.id);
        imported += 1;
    }
    if imported > 0 {
        tenant
            .publish(
                &mut tx,
                "product.media_changed",
                &serde_json::json!({ "product_id": product, "file_ids": ids }),
            )
            .await?;
    }
    let media = soat_files::views(&mut tx, &tenant, &storage, &ids).await?;
    tx.commit().await?;
    Ok(Json(MediaImported {
        imported,
        skipped,
        problems,
        media,
    }))
}
