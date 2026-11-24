//! F9: the plugins each shop turns on, and its settings for them.
//!
//! The install offers plugins (hooks that are `per_shop`); a shop sees
//! them at `GET /api/plugins`, turns one on with its settings at
//! `PUT /api/plugins/{name}`, and checkout then runs it for that shop
//! only. Secret settings are sealed and never shown again.
//!
//! F10: a shop can also upload its own plugin (`POST /api/plugins/uploads`).
//! It is listed and turned on like the install's, and runs for that shop
//! alone, in the same sandbox, after the install's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use soat_core::hooks::{fingerprint, Hook, Hooks, PluginLoader};
use soat_core::secret::Sealer;
use soat_core::Permission;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::multipart::Multipart;
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

/// Largest plugin component a shop may upload.
pub const MAX_PLUGIN_BYTES: usize = 8 << 20;

/// Compiled plugins kept at once; past this the cache starts over.
const COMPILED_KEPT: usize = 256;

/// Compiles the plugins shops upload, and keeps them compiled by the
/// fingerprints of their manifest and component; an app state. Without a loader (an install built without
/// the plugin host) uploads are refused.
#[derive(Clone, Default)]
pub struct PluginUploads {
    loader: Option<Arc<dyn PluginLoader>>,
    compiled: Arc<Mutex<HashMap<String, Arc<dyn Hook>>>>,
}

impl PluginUploads {
    pub fn new(loader: Arc<dyn PluginLoader>) -> Self {
        Self {
            loader: Some(loader),
            compiled: Arc::default(),
        }
    }

    /// The cache key: the same component under another manifest is
    /// another plugin.
    fn key(sha: &str, manifest: &str) -> String {
        format!("{sha}:{}", fingerprint(manifest.as_bytes()))
    }

    fn cached(&self, sha: &str, manifest: &str) -> Option<Arc<dyn Hook>> {
        self.compiled
            .lock()
            .ok()?
            .get(&Self::key(sha, manifest))
            .cloned()
    }

    fn compile(&self, sha: &str, manifest: &str, wasm: &[u8]) -> Result<Arc<dyn Hook>, String> {
        if let Some(hook) = self.cached(sha, manifest) {
            return Ok(hook);
        }
        let loader = self
            .loader
            .as_ref()
            .ok_or("this install runs no uploaded plugins")?;
        let hook = loader.load(manifest, wasm)?;
        if let Ok(mut kept) = self.compiled.lock() {
            if kept.len() >= COMPILED_KEPT {
                kept.clear();
            }
            kept.insert(Self::key(sha, manifest), hook.clone());
        }
        Ok(hook)
    }
}

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

/// A plugin this install offers, or one the shop uploaded, as the shop
/// has it.
#[model]
pub struct ShopPlugin {
    pub name: String,
    pub enabled: bool,
    pub settings: Vec<PluginSettingView>,
    /// The shop uploaded it (it runs for this shop alone).
    #[serde(default)]
    pub uploaded: bool,
    /// The uploaded package's version, from its manifest.
    #[serde(default)]
    pub version: Option<String>,
}

/// What `plugin.uploaded` and `plugin.removed` carry: never the component.
#[model]
pub struct PluginPackage {
    pub name: String,
    pub version: String,
    /// Hex SHA-256 of the component.
    pub sha256: String,
    pub size: i64,
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
    app.mount((list_plugins, set_plugin, upload_plugin, remove_plugin))
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

/// An uploaded plugin as listed: name, version and the hook, if it runs.
struct Uploaded {
    name: String,
    version: String,
    hook: Option<Arc<dyn Hook>>,
}

/// The plugins the shop uploaded, in name order, compiled (from the cache
/// when it can). One that no longer compiles is listed without a hook and
/// does not run.
async fn uploaded(
    conn: &mut PgConnection,
    tenant_id: &str,
    uploads: &PluginUploads,
) -> Result<Vec<Uploaded>> {
    if uploads.loader.is_none() {
        return Ok(Vec::new());
    }
    let found: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT name, version, sha256, manifest FROM shop_plugin_packages \
         WHERE tenant_id = $1 ORDER BY name",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(found.len());
    for (name, version, sha, manifest) in found {
        let hook = match uploads.cached(&sha, &manifest) {
            Some(hook) => Some(hook),
            None => {
                let wasm: Vec<u8> = sqlx::query_scalar(
                    "SELECT component FROM shop_plugin_packages \
                     WHERE tenant_id = $1 AND name = $2",
                )
                .bind(tenant_id)
                .bind(&name)
                .fetch_one(&mut *conn)
                .await?;
                uploads.compile(&sha, &manifest, &wasm).ok()
            }
        };
        out.push(Uploaded {
            name,
            version,
            hook,
        });
    }
    Ok(out)
}

/// The hooks `scope`'s shop runs: the install's, less the plugins it has
/// not turned on, then the ones it uploaded and turned on, with its
/// settings.
pub async fn shop_hooks(
    conn: &mut PgConnection,
    scope: &impl Scope,
    hooks: &Hooks,
    sealer: &PluginSecrets,
    uploads: &PluginUploads,
) -> Result<Hooks> {
    let mut hooks = hooks.clone();
    for u in uploaded(conn, scope.tenant_id(), uploads).await? {
        if let Some(hook) = u.hook {
            hooks = hooks.with_shared(hook);
        }
    }
    if !hooks.has_per_shop() {
        return Ok(hooks);
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
        uploaded: false,
        version: None,
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
    uploads: State<PluginUploads>,
) -> Result<Json<Vec<ShopPlugin>>> {
    let mut tx = tenant.begin_for(&db, &PLUGINS_READ).await?;
    let found = rows(&mut tx, tenant.id()).await?;
    let own = uploaded(&mut tx, tenant.id(), &uploads).await?;
    tx.commit().await?;
    let shown = |h: &dyn Hook| {
        let row = found.get(h.name());
        let secrets = open_secrets(&sealer, row.and_then(|r| r.secrets.as_deref()), tenant.id());
        view(h, row, &secrets)
    };
    let mut out: Vec<ShopPlugin> = hooks
        .iter()
        .filter(|h| h.per_shop())
        .map(|h| shown(h.as_ref()))
        .collect();
    for u in own {
        let mut v = match &u.hook {
            Some(hook) => shown(hook.as_ref()),
            // It no longer compiles: listed, so the shop can replace it.
            None => ShopPlugin {
                name: u.name.clone(),
                enabled: false,
                settings: Vec::new(),
                uploaded: true,
                version: None,
            },
        };
        v.uploaded = true;
        v.version = Some(u.version);
        out.push(v);
    }
    Ok(Json(out))
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
    uploads: State<PluginUploads>,
    Path(name): Path<String>,
    Json(b): Json<SetPlugin>,
) -> Result<Json<ShopPlugin>> {
    let mut tx = tenant.begin_for(&db, &PLUGINS_MANAGE).await?;
    let own = uploaded(&mut tx, tenant.id(), &uploads)
        .await?
        .into_iter()
        .find(|u| u.name == name);
    let (hook, version) = match own {
        Some(u) => (
            u.hook.ok_or_else(|| {
                Error::unprocessable(format!("{name} no longer loads: upload it again"))
                    .with_code("plugin_broken")
            })?,
            Some(u.version),
        ),
        None => (
            hooks
                .iter()
                .find(|h| h.per_shop() && h.name() == name)
                .cloned()
                .ok_or_else(Error::not_found)?,
            None,
        ),
    };
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
    let mut saved = view(hook.as_ref(), Some(&row), &secrets);
    saved.uploaded = version.is_some();
    saved.version = version;
    tenant.publish(&mut tx, "plugin.configured", &saved).await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// A manifest's `name` and `version`: what the shop's list shows; the
/// plugin host checks the rest.
fn name_and_version(manifest: &str) -> Result<(String, String)> {
    let bad = |m: &str| Error::unprocessable(m.to_owned()).with_code("invalid_plugin");
    let v: serde_json::Value =
        serde_json::from_str(manifest).map_err(|e| bad(&format!("manifest: {e}")))?;
    let field = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .ok_or_else(|| bad(&format!("the manifest has no {k}")))
    };
    let name = field("name")?;
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        return Err(bad(
            "a plugin name is 1 to 64 lowercase letters, digits, - or _",
        ));
    }
    Ok((name, field("version")?))
}

/// Upload a plugin for this shop alone (`plugins:manage`): `multipart/form-data`
/// fields `manifest` (its `soat.plugin.json`) and `component` (the `.wasm`
/// component, at most 8 MiB). It must target a checkout world
/// (`discounts`, `shipping-filter` or `checkout-validation`) and load in
/// the sandbox, or it is a 422 `invalid_plugin`. A new plugin starts turned
/// off; uploading one the shop has again replaces it and keeps its
/// settings. A name one of the install's plugins uses is a 409
/// `plugin_exists`. Publishes `plugin.uploaded` (never the component).
#[post("/api/plugins/uploads", tag = "plugins", status = 201)]
async fn upload_plugin(
    db: Db,
    tenant: Tenant,
    hooks: State<Hooks>,
    sealer: State<PluginSecrets>,
    uploads: State<PluginUploads>,
    mut form: Multipart,
) -> Result<(StatusCode, Json<ShopPlugin>)> {
    // Check the caller before reading the body.
    tenant
        .begin_for(&db, &PLUGINS_MANAGE)
        .await?
        .rollback()
        .await?;
    if uploads.loader.is_none() {
        return Err(Error::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this install runs no uploaded plugins (built without the plugin host)",
        )
        .with_code("plugins_unavailable"));
    }
    let mut manifest = None;
    let mut component = None;
    while let Some(field) = form.next_field().await? {
        match field.name() {
            Some("manifest") => manifest = Some(field.text().await?),
            Some("component") => component = Some(field.bytes().await?),
            _ => {}
        }
    }
    let (Some(manifest), Some(component)) = (manifest, component) else {
        return Err(Error::bad_request(
            "send the fields `manifest` (soat.plugin.json) and `component` (the .wasm)",
        )
        .with_code("missing_file"));
    };
    if component.len() > MAX_PLUGIN_BYTES {
        return Err(Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "a plugin component is at most 8 MiB",
        )
        .with_code("plugin_too_large"));
    }
    let (name, version) = name_and_version(&manifest)?;
    if hooks.iter().any(|h| h.name() == name) {
        return Err(
            Error::conflict(format!("this install already has a plugin named {name}"))
                .with_code("plugin_exists"),
        );
    }
    let sha = fingerprint(&component);
    // Compiling is CPU work: off the async threads.
    let hook = {
        let (uploads, sha, manifest, wasm) = (
            uploads.0.clone(),
            sha.clone(),
            manifest.clone(),
            component.clone(),
        );
        tokio::task::spawn_blocking(move || uploads.compile(&sha, &manifest, &wasm))
            .await
            .map_err(|_| Error::internal("compiling the plugin failed"))?
            .map_err(|e| Error::unprocessable(e).with_code("invalid_plugin"))?
    };
    if hook.name() != name {
        return Err(
            Error::unprocessable("the component's name is not the manifest's")
                .with_code("invalid_plugin"),
        );
    }
    let mut tx = tenant.begin_for(&db, &PLUGINS_MANAGE).await?;
    let replaced = sqlx::query_scalar::<_, bool>(
        "INSERT INTO shop_plugin_packages (tenant_id, name, version, manifest, component, sha256, size) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tenant_id, name) DO UPDATE SET \
         version = EXCLUDED.version, manifest = EXCLUDED.manifest, \
         component = EXCLUDED.component, sha256 = EXCLUDED.sha256, size = EXCLUDED.size, \
         updated_at = now() RETURNING (xmax <> 0)",
    )
    .bind(tenant.id())
    .bind(&name)
    .bind(&version)
    .bind(&manifest)
    .bind(component.as_ref())
    .bind(&sha)
    .bind(component.len() as i64)
    .fetch_one(&mut *tx)
    .await?;
    let package = PluginPackage {
        name: name.clone(),
        version: version.clone(),
        sha256: sha,
        size: component.len() as i64,
    };
    tenant.publish(&mut tx, "plugin.uploaded", &package).await?;
    let row = rows(&mut tx, tenant.id()).await?.remove(&name);
    tx.commit().await?;
    let secrets = open_secrets(
        &sealer,
        row.as_ref().and_then(|r| r.secrets.as_deref()),
        tenant.id(),
    );
    let mut saved = view(hook.as_ref(), row.as_ref(), &secrets);
    saved.uploaded = true;
    saved.version = Some(version);
    let status = if replaced {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(saved)))
}

/// Remove a plugin the shop uploaded, and its settings (`plugins:manage`).
/// Publishes `plugin.removed`.
#[delete("/api/plugins/uploads/{name}", tag = "plugins", status = 204)]
async fn remove_plugin(db: Db, tenant: Tenant, Path(name): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PLUGINS_MANAGE).await?;
    let gone: Option<(String, String, i64)> = sqlx::query_as(
        "DELETE FROM shop_plugin_packages WHERE tenant_id = $1 AND name = $2 \
         RETURNING version, sha256, size",
    )
    .bind(tenant.id())
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await?;
    let (version, sha256, size) = gone.ok_or_else(Error::not_found)?;
    sqlx::query("DELETE FROM shop_plugins WHERE tenant_id = $1 AND name = $2")
        .bind(tenant.id())
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    let package = PluginPackage {
        name,
        version,
        sha256,
        size,
    };
    tenant.publish(&mut tx, "plugin.removed", &package).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
