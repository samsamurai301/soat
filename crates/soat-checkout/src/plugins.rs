//! F9: the plugins each shop turns on, and its settings for them.
//!
//! The install offers plugins (hooks that are `per_shop`); a shop sees
//! them at `GET /api/plugins`, turns one on with its settings at
//! `PUT /api/plugins/{name}`, and checkout then runs it for that shop
//! only. Secret settings are sealed and never shown again.

use std::collections::HashMap;

use serde_json::json;
use soat_core::hooks::{Hook, Hooks};
use soat_core::secret::Sealer;
use soat_core::Permission;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

/// See the plugins this install offers and which the shop turned on.
pub const PLUGINS_READ: Permission =
    Permission::new("plugins:read", "See the shop's plugins").for_staff();
/// Turn plugins on or off and change their settings. Not given to staff
/// by default.
pub const PLUGINS_MANAGE: Permission =
    Permission::new("plugins:manage", "Turn plugins on and off and set them up");

/// Seals the shops' secret plugin settings; an app state.
#[derive(Clone, Default)]
pub struct PluginSecrets(pub Option<Sealer>);

/// One setting of a plugin, as the shop has it.
#[model]
pub struct PluginSettingView {
    pub key: String,
    pub label: String,
    pub secret: bool,
    /// The shop's value; never shown for a secret.
    pub value: Option<String>,
    /// Whether the shop gave it a value.
    pub set: bool,
}

/// A plugin this install offers, as the shop has it.
#[model]
pub struct ShopPlugin {
    pub name: String,
    pub enabled: bool,
    pub settings: Vec<PluginSettingView>,
}

/// Body of `PUT /api/plugins/{name}`.
#[model]
pub struct SetPlugin {
    pub enabled: bool,
    /// Values by key; a secret left out or empty keeps the one stored.
    #[serde(default)]
    pub settings: HashMap<String, String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((list_plugins, set_plugin))
}

/// A `shop_plugins` row: name, enabled, settings, sealed secrets.
type StoredRow = (
    String,
    bool,
    SqlJson<HashMap<String, String>>,
    Option<Vec<u8>>,
);

struct Row {
    enabled: bool,
    settings: HashMap<String, String>,
    secrets: Option<Vec<u8>>,
}

async fn rows(conn: &mut PgConnection, tenant_id: &str) -> Result<HashMap<String, Row>> {
    let found: Vec<StoredRow> = sqlx::query_as(
        "SELECT name, enabled, settings, secrets FROM shop_plugins WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(conn)
    .await?;
    Ok(found
        .into_iter()
        .map(|(name, enabled, settings, secrets)| {
            (
                name,
                Row {
                    enabled,
                    settings: settings.0,
                    secrets,
                },
            )
        })
        .collect())
}

fn open_secrets(
    sealer: &PluginSecrets,
    sealed: Option<&[u8]>,
    tenant_id: &str,
) -> HashMap<String, String> {
    sealed
        .and_then(|b| sealer.0.as_ref()?.open(b, tenant_id))
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The hooks `scope`'s shop runs: the install's, less the plugins it has
/// not turned on, with its settings.
pub async fn shop_hooks(
    conn: &mut PgConnection,
    scope: &impl Scope,
    hooks: &Hooks,
    sealer: &PluginSecrets,
) -> Result<Hooks> {
    if !hooks.has_per_shop() {
        return Ok(hooks.clone());
    }
    let on = rows(conn, scope.tenant_id())
        .await?
        .into_iter()
        .filter(|(_, r)| r.enabled)
        .map(|(name, r)| {
            let mut settings = r.settings;
            settings.extend(open_secrets(
                sealer,
                r.secrets.as_deref(),
                scope.tenant_id(),
            ));
            (name, settings)
        })
        .collect();
    Ok(hooks.for_shop(&on))
}

fn view(hook: &dyn Hook, row: Option<&Row>, secrets: &HashMap<String, String>) -> ShopPlugin {
    ShopPlugin {
        name: hook.name().to_owned(),
        enabled: row.is_some_and(|r| r.enabled),
        settings: hook
            .settings()
            .into_iter()
            .map(|s| {
                let value = if s.secret {
                    None
                } else {
                    row.and_then(|r| r.settings.get(&s.key).cloned())
                };
                let set = value.is_some() || (s.secret && secrets.contains_key(&s.key));
                PluginSettingView {
                    key: s.key,
                    label: s.label,
                    secret: s.secret,
                    value,
                    set,
                }
            })
            .collect(),
    }
}

/// The plugins this install offers, and which of them the shop turned on
/// (`plugins:read`). Secret settings only say whether they are set.
#[get("/api/plugins", tag = "plugins")]
async fn list_plugins(
    db: Db,
    tenant: Tenant,
    hooks: State<Hooks>,
    sealer: State<PluginSecrets>,
) -> Result<Json<Vec<ShopPlugin>>> {
    let mut tx = tenant.begin_for(&db, &PLUGINS_READ).await?;
    let found = rows(&mut tx, tenant.id()).await?;
    tx.commit().await?;
    Ok(Json(
        hooks
            .iter()
            .filter(|h| h.per_shop())
            .map(|h| {
                let row = found.get(h.name());
                let secrets =
                    open_secrets(&sealer, row.and_then(|r| r.secrets.as_deref()), tenant.id());
                view(h.as_ref(), row, &secrets)
            })
            .collect(),
    ))
}

/// Turn a plugin on or off for the shop and set it up (`plugins:manage`).
/// Settings it does not ask for are refused; secret ones are sealed, and a
/// secret left out keeps the one stored. Publishes `plugin.configured`,
/// without secrets.
#[put("/api/plugins/{name}", tag = "plugins")]
async fn set_plugin(
    db: Db,
    tenant: Tenant,
    hooks: State<Hooks>,
    sealer: State<PluginSecrets>,
    Path(name): Path<String>,
    Json(b): Json<SetPlugin>,
) -> Result<Json<ShopPlugin>> {
    let mut tx = tenant.begin_for(&db, &PLUGINS_MANAGE).await?;
    let hook = hooks
        .iter()
        .find(|h| h.per_shop() && h.name() == name)
        .ok_or_else(Error::not_found)?;
    let specs = hook.settings();
    let mut plain = HashMap::new();
    let mut given_secrets = HashMap::new();
    for (key, value) in b.settings {
        let Some(spec) = specs.iter().find(|s| s.key == key) else {
            return Err(
                Error::unprocessable(format!("{name} has no setting {key:?}"))
                    .with_code("unknown_setting"),
            );
        };
        if value.chars().count() > 2000 {
            return Err(Error::unprocessable(format!("{key} is too long")));
        }
        if spec.secret {
            if !value.is_empty() {
                given_secrets.insert(key, value);
            }
        } else {
            plain.insert(key, value);
        }
    }
    let before = rows(&mut tx, tenant.id()).await?.remove(&name);
    let mut secrets = open_secrets(
        &sealer,
        before.as_ref().and_then(|r| r.secrets.as_deref()),
        tenant.id(),
    );
    let changed_secrets = !given_secrets.is_empty();
    secrets.extend(given_secrets);
    let sealed = if secrets.is_empty() {
        None
    } else if changed_secrets {
        let s = sealer.0 .0.as_ref().ok_or_else(|| {
            Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "this install keeps no secrets: set SOAT_SECRET_KEY",
            )
            .with_code("secrets_unavailable")
        })?;
        Some(s.seal(json!(secrets).to_string().as_bytes(), tenant.id()))
    } else {
        before.as_ref().and_then(|r| r.secrets.clone())
    };
    sqlx::query(
        "INSERT INTO shop_plugins (tenant_id, name, enabled, settings, secrets) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, name) DO UPDATE SET \
         enabled = EXCLUDED.enabled, settings = EXCLUDED.settings, \
         secrets = EXCLUDED.secrets, updated_at = now()",
    )
    .bind(tenant.id())
    .bind(&name)
    .bind(b.enabled)
    .bind(SqlJson(&plain))
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    let row = Row {
        enabled: b.enabled,
        settings: plain,
        secrets: None,
    };
    let saved = view(hook.as_ref(), Some(&row), &secrets);
    tenant.publish(&mut tx, "plugin.configured", &saved).await?;
    tx.commit().await?;
    Ok(Json(saved))
}
