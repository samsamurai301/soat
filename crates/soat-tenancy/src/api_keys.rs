//! API keys: how scripts, apps and Claude act for a shop.
//!
//! A key looks like `soat_sk_<shop>_<secret>` and is sent as a bearer token;
//! no `x-soat-tenant` header is needed. It grants exactly the permissions
//! chosen at creation, which must all be held by the person creating it.
//! Only signed-in people create keys, so a leaked key cannot mint more.

use soat_core::{id_kind, Permission};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{Tenant, API_KEY_PREFIX};

id_kind!(ApiKeyId, ApiKeyKind, "key");

/// Create, list and revoke the shop's API keys.
pub const API_KEYS_MANAGE: Permission =
    Permission::new("api_keys:manage", "Create, list and revoke API keys");

/// A key to create.
#[model]
pub struct NewApiKey {
    /// What the key is for, e.g. `Claude` or `ERP sync`.
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// Permission names the key grants.
    pub permissions: Vec<String>,
}

/// An API key as listed. The secret is never shown again.
#[model]
#[derive(sqlx::FromRow)]
pub struct ApiKeyView {
    pub id: String,
    pub name: String,
    pub permissions: Vec<String>,
    pub revoked: bool,
}

/// A newly created key, with its secret. Store it now: it is shown once.
#[model]
pub struct CreatedApiKey {
    pub key: ApiKeyView,
    pub secret: String,
}

/// Stores a new key of shop `tenant_id` granting `permissions`, made by
/// `created_by` (a user, or an app install's consenting user), in the
/// shop's transaction. Returns it with its secret, which is never stored.
/// Permissions are not checked here: callers check them.
///
/// # Errors
///
/// Database errors.
pub async fn insert_key(
    conn: &mut zapline::db::sqlx::PgConnection,
    tenant_id: &str,
    name: &str,
    mut permissions: Vec<String>,
    created_by: Option<&str>,
) -> Result<(ApiKeyView, String)> {
    permissions.sort();
    permissions.dedup();
    let id = ApiKeyId::new();
    let secret = format!("{API_KEY_PREFIX}{tenant_id}_{}", new_token()?);
    sqlx::query(
        "INSERT INTO api_keys (id, tenant_id, name, token_hash, permissions, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.as_str())
    .bind(tenant_id)
    .bind(name)
    .bind(hash_token(&secret))
    .bind(&permissions)
    .bind(created_by)
    .execute(conn)
    .await?;
    Ok((
        ApiKeyView {
            id: id.to_string(),
            name: name.to_owned(),
            permissions,
            revoked: false,
        },
        secret,
    ))
}

/// Revokes key `id` in the shop's transaction; `false` when it is unknown
/// or revoked already.
///
/// # Errors
///
/// Database errors.
pub async fn revoke(conn: &mut zapline::db::sqlx::PgConnection, id: &str) -> Result<bool> {
    let done =
        sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(conn)
            .await?;
    Ok(done.rows_affected() > 0)
}

pub fn mount(app: App) -> App {
    app.mount((create_key, list_keys, revoke_key))
}

/// Create an API key (`api_keys:manage`, signed-in people only). Every
/// permission it grants must be one you hold.
#[post("/api/api-keys", tag = "api keys", status = 201)]
async fn create_key(
    db: Db,
    tenant: Tenant,
    Valid(Json(body)): Valid<Json<NewApiKey>>,
) -> Result<(StatusCode, Json<CreatedApiKey>)> {
    let Some(staff) = tenant.staff().cloned() else {
        return Err(Error::forbidden().with_code("people_only"));
    };
    if let Some(unknown) = body
        .permissions
        .iter()
        .find(|p| tenant.permissions().get(p).is_none())
    {
        return Err(
            Error::unprocessable(format!("unknown permission {unknown:?}"))
                .with_code("unknown_permission"),
        );
    }
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&API_KEYS_MANAGE)?;
    let beyond: Vec<_> = body
        .permissions
        .iter()
        .filter(|p| !access.allows(p))
        .map(String::as_str)
        .collect();
    if !beyond.is_empty() {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            format!(
                "the key would grant permissions you lack: {}",
                beyond.join(", ")
            ),
        )
        .with_code("beyond_own_permissions"));
    }
    let (key, secret) = insert_key(
        &mut tx,
        tenant.id(),
        &body.name,
        body.permissions,
        Some(staff.user_id.as_str()),
    )
    .await?;
    // The event carries the key's view, never its secret.
    tenant.publish(&mut tx, "api_key.created", &key).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(CreatedApiKey { key, secret })))
}

/// The shop's API keys, newest first (`api_keys:manage`).
#[get("/api/api-keys", tag = "api keys")]
async fn list_keys(db: Db, tenant: Tenant) -> Result<Json<Vec<ApiKeyView>>> {
    let mut tx = tenant.begin_for(&db, &API_KEYS_MANAGE).await?;
    let keys = sqlx::query_as::<_, ApiKeyView>(
        "SELECT id, name, permissions, revoked_at IS NOT NULL AS revoked \
         FROM api_keys ORDER BY id DESC",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(keys))
}

/// Revoke an API key at once (`api_keys:manage`).
#[delete("/api/api-keys/{id}", tag = "api keys", status = 204)]
async fn revoke_key(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &API_KEYS_MANAGE).await?;
    if !revoke(&mut tx, &id).await? {
        return Err(Error::not_found());
    }
    tenant
        .publish(&mut tx, "api_key.revoked", &serde_json::json!({ "id": id }))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
