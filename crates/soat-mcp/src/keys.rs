//! A shop's own Anthropic API key, so SOAT can call Claude for the shop.
//! The key is sealed with the server's secret (`SOAT_SECRET_KEY`), bound to
//! the shop, and never shown again; events carry its last four characters.

use soat_core::secret::Sealer;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::{self, PgConnection};
use zapline::db::Db;
use zapline::prelude::*;

use crate::{Shared, ASSISTANT_MANAGE};

/// Whether the shop has a key, and which.
#[model]
#[derive(sqlx::FromRow)]
pub struct KeyView {
    pub set: bool,
    pub last4: Option<String>,
    pub model: Option<String>,
    /// RFC 3339, UTC.
    pub updated_at: Option<String>,
}

/// A key to store.
#[model]
pub struct NewKey {
    /// An Anthropic API key, `sk-ant-...`.
    #[validate(length(min = 20, max = 300))]
    pub api_key: String,
    /// The Claude model to call, as Anthropic names it.
    #[validate(length(min = 1, max = 100))]
    pub model: String,
}

/// A shop's key and model, opened.
#[derive(Clone)]
pub struct AnthropicKey {
    pub api_key: String,
    pub model: String,
}

impl std::fmt::Debug for AnthropicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AnthropicKey(…, {})", self.model)
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_key, put_key, delete_key))
}

fn sealer(s: &Shared) -> Result<&Sealer> {
    s.sealer.as_ref().ok_or_else(|| {
        Error::conflict("this server cannot store keys: set SOAT_SECRET_KEY and restart it")
            .with_code("no_secret_key")
    })
}

async fn view(conn: &mut PgConnection, scope: &impl Scope) -> Result<KeyView> {
    let row = sqlx::query_as::<_, KeyView>(
        "SELECT true AS set, last4, model, \
         to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at \
         FROM assistant_keys WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.unwrap_or(KeyView {
        set: false,
        last4: None,
        model: None,
        updated_at: None,
    }))
}

/// The shop's key and model, if it stored one that this server can open.
pub async fn anthropic_key(
    conn: &mut PgConnection,
    scope: &impl Scope,
    sealer: &Sealer,
) -> Result<Option<AnthropicKey>> {
    let row: Option<(Vec<u8>, String)> =
        sqlx::query_as("SELECT sealed, model FROM assistant_keys WHERE tenant_id = $1")
            .bind(scope.tenant_id())
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.and_then(|(sealed, model)| {
        let key = sealer.open(&sealed, scope.tenant_id())?;
        Some(AnthropicKey {
            api_key: String::from_utf8(key).ok()?,
            model,
        })
    }))
}

/// Whether the shop has its own Anthropic key, its last four characters and
/// model; never the key (`assistant:manage`).
#[get("/api/assistant/key", tag = "assistant")]
async fn get_key(db: Db, tenant: Tenant) -> Result<Json<KeyView>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_MANAGE).await?;
    let v = view(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(v))
}

/// Store the shop's Anthropic API key and the model to call
/// (`assistant:manage`), replacing any before. 409 `no_secret_key` when the
/// server has no `SOAT_SECRET_KEY`. Publishes `assistant_key.set` with the
/// last four characters only.
#[put("/api/assistant/key", tag = "assistant")]
async fn put_key(
    db: Db,
    tenant: Tenant,
    s: State<Shared>,
    Valid(Json(k)): Valid<Json<NewKey>>,
) -> Result<Json<KeyView>> {
    let key = k.api_key.trim();
    if !key.starts_with("sk-ant-") || key.chars().any(char::is_whitespace) {
        return Err(
            Error::unprocessable("an Anthropic API key starts with sk-ant-")
                .with_code("invalid_key"),
        );
    }
    let model = k.model.trim();
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '@' | ':'))
    {
        return Err(
            Error::unprocessable("a model name is letters, digits, - . _ @ and :")
                .with_code("invalid_model"),
        );
    }
    let mut tx = tenant.begin_for(&db, &ASSISTANT_MANAGE).await?;
    let sealed = sealer(&s)?.seal(key.as_bytes(), tenant.id());
    let last4: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    sqlx::query(
        "INSERT INTO assistant_keys (tenant_id, sealed, last4, model) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (tenant_id) DO UPDATE SET sealed = EXCLUDED.sealed, last4 = EXCLUDED.last4, \
         model = EXCLUDED.model, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(&sealed)
    .bind(&last4)
    .bind(model)
    .execute(&mut *tx)
    .await?;
    let v = view(&mut tx, &tenant).await?;
    tenant.publish(&mut tx, "assistant_key.set", &v).await?;
    tx.commit().await?;
    Ok(Json(v))
}

/// Forget the shop's Anthropic key (`assistant:manage`). Publishes
/// `assistant_key.removed`.
#[delete("/api/assistant/key", tag = "assistant", status = 204)]
async fn delete_key(db: Db, tenant: Tenant) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM assistant_keys WHERE tenant_id = $1")
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(&mut tx, "assistant_key.removed", &serde_json::json!({}))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
