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
    let mut permissions = body.permissions;
    permissions.sort();
    permissions.dedup();
    let id = ApiKeyId::new();
    let secret = format!("{API_KEY_PREFIX}{}_{}", tenant.id(), new_token()?);
    sqlx::query(
        "INSERT INTO api_keys (id, tenant_id, name, token_hash, permissions, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(&body.name)
    .bind(hash_token(&secret))
    .bind(&permissions)
    .bind(staff.user_id.as_str())
    .execute(&mut *tx)
    .await?;
    let key = ApiKeyView {
        id: id.to_string(),
        name: body.name,
        permissions,
        revoked: false,
    };
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
    let revoked =
        sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    if revoked.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(&mut tx, "api_key.revoked", &serde_json::json!({ "id": id }))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
