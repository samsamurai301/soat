//! Files and media.
//!
//! `POST /api/files` takes one `multipart/form-data` field `file` (and an
//! optional `alt`). The type comes from the file's first bytes
//! ([`soat_core::media::detect`]): images, PDF, video, and MP3, ZIP and
//! EPUB for downloads; never HTML or SVG. Bytes go to Zapline [`Storage`]
//! (a local directory, or S3 with `S3_BUCKET`); every view carries a
//! signed `url` valid for an hour.
//!
//! Modules that attach files (product media, CMS images) keep the file ids
//! and call [`views`] to show them.

use std::time::Duration;

use soat_core::media::{detect, safe_file_name};
use soat_core::{id_kind, Permission};
use soat_module::Module;
use soat_tenancy::{Tenant, TenantListing};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::multipart::{Multipart, MultipartConfig};
use zapline::prelude::*;
use zapline::storage::{PutOptions, Storage};

id_kind!(FileId, FileKind, "file");

/// Largest upload.
pub const MAX_UPLOAD: usize = 20 * 1024 * 1024;
/// How long a file's `url` works.
pub const LINK_TTL: Duration = Duration::from_secs(3600);

/// See the shop's files.
pub const FILES_READ: Permission =
    Permission::new("files:read", "See the shop's files").for_staff();
/// Upload, describe and delete files.
pub const FILES_WRITE: Permission =
    Permission::new("files:write", "Upload, describe and delete files").for_staff();

/// The files module, storing bytes in `storage`.
pub struct Files {
    storage: Storage,
}

impl Files {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }
}

impl Module for Files {
    fn name(&self) -> &'static str {
        "files"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[FILES_READ, FILES_WRITE]
    }

    fn mount(&self, app: App) -> App {
        app.storage(self.storage.clone())
            .state(
                MultipartConfig::new()
                    .body_limit(MAX_UPLOAD + 64 * 1024)
                    .max_field_size(MAX_UPLOAD as u64)
                    .max_fields(4),
            )
            .mount((upload, list_files, get_file, update_file, delete_file))
    }
}

/// A file of the shop.
#[model]
pub struct FileView {
    /// e.g. `file_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// A safe version of the uploaded name, e.g. `red-shoe.jpg`.
    pub file_name: String,
    /// Detected from the bytes, e.g. `image/jpeg`.
    pub content_type: String,
    /// In bytes.
    pub size: i64,
    /// Alternative text for images.
    pub alt: String,
    /// A link to the bytes, valid for an hour.
    pub url: String,
}

/// Body of an update request.
#[model]
pub struct FileChange {
    #[validate(length(max = 1000))]
    pub alt: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    key: String,
    file_name: String,
    content_type: String,
    size: i64,
    alt: String,
}

impl Row {
    fn view(self, storage: &Storage) -> FileView {
        FileView {
            url: storage.url(&self.key, LINK_TTL),
            id: self.id,
            file_name: self.file_name,
            content_type: self.content_type,
            size: self.size,
            alt: self.alt,
        }
    }
}

const COLUMNS: &str = "id, key, file_name, content_type, size, alt";

async fn row(conn: &mut PgConnection, tenant: &Tenant, id: &str) -> Result<Row> {
    let id = FileId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM files WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// Upload a file (`files:write`): a `multipart/form-data` field `file`, up
/// to 20 MiB, and optionally `alt`. PNG, JPEG, GIF, WebP, AVIF, PDF, MP4,
/// WebM, MP3, ZIP and EPUB only, recognized by content: anything else is a
/// 415 `unsupported_file_type`. Publishes `file.created`.
#[post("/api/files", tag = "files", status = 201)]
async fn upload(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    mut form: Multipart,
) -> Result<(StatusCode, Json<FileView>)> {
    // Check the caller before reading the body.
    tenant
        .begin_for(&db, &FILES_WRITE)
        .await?
        .rollback()
        .await?;
    let mut file = None;
    let mut alt = String::new();
    while let Some(field) = form.next_field().await? {
        match field.name() {
            Some("file") => {
                let name = field.file_name().unwrap_or("file").to_owned();
                file = Some((name, field.bytes().await?));
            }
            Some("alt") => alt = field.text().await?,
            _ => {}
        }
    }
    let Some((name, bytes)) = file else {
        return Err(
            Error::bad_request("send the file in a field named `file`").with_code("missing_file")
        );
    };
    if alt.chars().count() > 1000 {
        return Err(Error::unprocessable("alt is at most 1000 characters").with_code("invalid_alt"));
    }
    let kind = detect(&bytes).ok_or_else(|| {
        Error::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "only PNG, JPEG, GIF, WebP, AVIF, PDF, MP4, WebM, MP3, ZIP and EPUB files",
        )
        .with_code("unsupported_file_type")
    })?;
    let id = FileId::new();
    let file_name = safe_file_name(&name, kind);
    let key = format!("{}/{}/{}", tenant.id(), id, file_name);
    let size = bytes.len() as i64;
    storage
        .put_with(&key, bytes, PutOptions::new().content_type(kind.mime))
        .await?;
    let saved = async {
        let mut tx = tenant.begin_for(&db, &FILES_WRITE).await?;
        let row = sqlx::query_as::<_, Row>(&format!(
            "INSERT INTO files (id, tenant_id, key, file_name, content_type, size, alt) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"
        ))
        .bind(id.as_str())
        .bind(tenant.id())
        .bind(&key)
        .bind(&file_name)
        .bind(kind.mime)
        .bind(size)
        .bind(&alt)
        .fetch_one(&mut *tx)
        .await?;
        let view = row.view(&storage);
        tenant.publish(&mut tx, "file.created", &view).await?;
        tx.commit().await?;
        Ok::<_, Error>(view)
    }
    .await;
    match saved {
        Ok(view) => Ok((StatusCode::CREATED, Json(view))),
        Err(e) => {
            // Leave no bytes without a row.
            let _ = storage.delete(&key).await;
            Err(e)
        }
    }
}

/// The shop's files, newest first, 50 per page (`files:read`). `?q=`
/// searches names and alt text; `?content_type=image/png` filters.
#[get("/api/files", tag = "files")]
async fn list_files(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    params: ListParams,
) -> Result<Json<Paged<FileView>>> {
    let mut tx = tenant.begin_for(&db, &FILES_READ).await?;
    let page: Paged<Row> = TenantListing::new("files", COLUMNS)
        .sortable(["file_name", "size"])
        .default_sort("-id")
        .search(["file_name", "alt"])
        .filter_text("content_type")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items: page.items.into_iter().map(|r| r.view(&storage)).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// One file, with a fresh link (`files:read`).
#[get("/api/files/{id}", tag = "files")]
async fn get_file(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<FileView>> {
    let mut tx = tenant.begin_for(&db, &FILES_READ).await?;
    let row = row(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(row.view(&storage)))
}

/// Change a file's alt text (`files:write`). Publishes `file.updated`.
#[patch("/api/files/{id}", tag = "files")]
async fn update_file(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
    Valid(Json(change)): Valid<Json<FileChange>>,
) -> Result<Json<FileView>> {
    let mut tx = tenant.begin_for(&db, &FILES_WRITE).await?;
    let current = row(&mut tx, &tenant, &id).await?;
    let updated = sqlx::query_as::<_, Row>(&format!(
        "UPDATE files SET alt = $3 WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&change.alt)
    .fetch_one(&mut *tx)
    .await?;
    let view = updated.view(&storage);
    tenant.publish(&mut tx, "file.updated", &view).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Delete a file; it disappears from every product and page using it
/// (`files:write`). Publishes `file.deleted`.
#[delete("/api/files/{id}", tag = "files", status = 204)]
async fn delete_file(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &FILES_WRITE).await?;
    let current = row(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM files WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "file.deleted",
            &serde_json::json!({ "id": current.id }),
        )
        .await?;
    tx.commit().await?;
    // The row is gone; a failed delete of the bytes only leaves garbage.
    let _ = storage.delete(&current.key).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Stores `bytes` as a file of the shop, named after `name`, in the
/// tenant transaction `tx`, and publishes `file.created`: for files a
/// module makes itself (a shipping label, say). A type SOAT does not store
/// is a 415 `unsupported_file_type`.
pub async fn store(
    tx: &mut soat_tenancy::TenantTx,
    scope: &impl soat_tenancy::Scope,
    storage: &Storage,
    name: &str,
    bytes: zapline::Bytes,
) -> Result<FileView> {
    let kind = detect(&bytes).ok_or_else(|| {
        Error::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "not a file type SOAT stores",
        )
        .with_code("unsupported_file_type")
    })?;
    let id = FileId::new();
    let file_name = safe_file_name(name, kind);
    let key = format!("{}/{}/{}", scope.tenant_id(), id, file_name);
    let size = bytes.len() as i64;
    storage
        .put_with(&key, bytes, PutOptions::new().content_type(kind.mime))
        .await?;
    let row = sqlx::query_as::<_, Row>(&format!(
        "INSERT INTO files (id, tenant_id, key, file_name, content_type, size, alt) \
         VALUES ($1, $2, $3, $4, $5, $6, '') RETURNING {COLUMNS}"
    ))
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(&key)
    .bind(&file_name)
    .bind(kind.mime)
    .bind(size)
    .fetch_one(&mut **tx)
    .await?;
    let view = row.view(storage);
    scope.publish(tx, "file.created", &view).await?;
    Ok(view)
}

/// The views of `ids`, in that order, skipping ids that are not the shop's
/// files. Run it in the tenant transaction.
pub async fn views(
    conn: &mut PgConnection,
    tenant: &impl soat_tenancy::Scope,
    storage: &Storage,
    ids: &[String],
) -> Result<Vec<FileView>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM files WHERE tenant_id = $1 AND id = ANY($2)"
    ))
    .bind(tenant.tenant_id())
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut rows: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    Ok(ids
        .iter()
        .filter_map(|id| {
            rows.iter_mut()
                .find(|r| r.as_ref().is_some_and(|r| &r.id == id))
                .and_then(Option::take)
        })
        .map(|r| r.view(storage))
        .collect())
}

/// How many of `ids` are the shop's files.
pub async fn count_owned(conn: &mut PgConnection, tenant: &Tenant, ids: &[String]) -> Result<i64> {
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM files WHERE tenant_id = $1 AND id = ANY($2)")
            .bind(tenant.id())
            .bind(ids)
            .fetch_one(&mut *conn)
            .await?;
    Ok(n)
}
