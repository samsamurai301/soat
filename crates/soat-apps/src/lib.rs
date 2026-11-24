//! Apps: software outside SOAT that works for shops, the way Shopify apps
//! do. A developer registers an app with the permissions it asks for and
//! where to send shops back to; a shop's owner installs it on a consent
//! screen that lists those permissions; the app trades the one-time code
//! it gets for an API key of that shop with exactly the permissions
//! granted (an OAuth 2 authorization code flow).
//!
//! ```text
//! developer: POST /api/apps                      -> client_id, client_secret
//! shop:      /admin/{shop}/apps/authorize?client_id=…&redirect_uri=…&state=…
//!            POST /api/app-installations         -> redirect_uri?code=…&shop=…&state=…
//! app:       POST /api/apps/token                -> soat_sk_… (the shop's key)
//! ```
//!
//! Uninstalling revokes the key at once and publishes `app.uninstalled`.

use soat_auth::Staff;
use soat_core::{id_kind, Permission, TenantId};
use soat_module::{Module, Permissions};
use soat_tenancy::api_keys::{insert_key, revoke};
use soat_tenancy::{Shop, Tenant};
use zapline::auth::password::{hash_token, new_token};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(AppId, AppKind, "app");
id_kind!(InstallationId, InstallationKind, "install");

/// Install apps, change what they may do, and uninstall them.
pub const APPS_MANAGE: Permission = Permission::new("apps:manage", "Install and uninstall apps");

/// How long a code waits to be traded for a key.
const CODE_MINUTES: i32 = 10;

/// The apps module.
pub struct Apps;

impl Module for Apps {
    fn name(&self) -> &'static str {
        "apps"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["auth", "tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[APPS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            register_app,
            list_apps,
            rotate_secret,
            consent,
            install,
            list_installations,
            uninstall,
            token,
        ))
    }
}

/// An app as its developer sees it.
#[model]
#[derive(sqlx::FromRow)]
pub struct AppView {
    /// The app's id, its OAuth `client_id`: `app_...`.
    pub id: String,
    pub name: String,
    /// Where shops are sent back to after consenting; exact matches only.
    pub redirect_uris: Vec<String>,
    /// The permissions it asks for.
    pub permissions: Vec<String>,
    pub created_at: String,
}

/// A new app, with its secret. Store it now: it is shown once.
#[model]
pub struct RegisteredApp {
    pub app: AppView,
    pub client_secret: String,
}

/// Body of `POST /api/apps`.
#[model]
pub struct NewApp {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    /// 1 to 5 addresses: `https://…`, or `http://localhost…` while
    /// developing.
    pub redirect_uris: Vec<String>,
    /// Permission names, e.g. `products:read`.
    pub permissions: Vec<String>,
}

/// A permission on the consent screen.
#[model]
pub struct AskedPermission {
    pub name: String,
    pub description: String,
    /// Whether the person consenting holds it: they can only grant what
    /// they hold.
    pub held: bool,
}

/// What a shop is asked to consent to.
#[model]
pub struct Consent {
    pub client_id: String,
    pub name: String,
    pub permissions: Vec<AskedPermission>,
    /// Whether the shop has it installed already.
    pub installed: bool,
}

/// Body of `POST /api/app-installations`.
#[model]
pub struct Install {
    pub client_id: String,
    /// One of the app's redirect addresses.
    #[validate(length(min = 1, max = 500))]
    pub redirect_uri: String,
    /// Handed back to the app unchanged, against forged redirects.
    #[validate(length(max = 500))]
    pub state: Option<String>,
}

/// An app a shop installed.
#[model]
#[derive(sqlx::FromRow)]
pub struct Installation {
    /// e.g. `install_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub app_id: String,
    pub app_name: String,
    /// What the app may do in this shop.
    pub permissions: Vec<String>,
    /// Whether the app has traded its code for a key.
    pub connected: bool,
    pub created_at: String,
}

/// An install, and where to send the person consenting.
#[model]
pub struct Installed {
    pub installation: Installation,
    /// The app's redirect address with `code`, `shop` and `state`.
    pub redirect_to: String,
}

/// Body of `POST /api/apps/token`.
#[model]
pub struct TokenRequest {
    pub client_id: String,
    pub client_secret: String,
    pub code: String,
    /// The shop the code came back with.
    pub shop: String,
    pub redirect_uri: String,
}

/// The shop's key for the app.
#[model]
pub struct AppToken {
    /// An API key (`soat_sk_…`): send it as `Authorization: Bearer`.
    pub access_token: String,
    pub shop: String,
    pub permissions: Vec<String>,
}

const APP_COLUMNS: &str = "id, name, redirect_uris, permissions, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

const INSTALL_COLUMNS: &str = "i.id, i.app_id, a.name AS app_name, i.permissions, \
     i.api_key_id IS NOT NULL AS connected, \
     to_char(i.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

fn invalid(message: impl Into<String>) -> Error {
    Error::unprocessable(message.into()).with_code("invalid_app")
}

/// `https://…`, or plain HTTP on this machine while developing; no
/// fragment.
fn is_redirect(uri: &str) -> bool {
    let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
        uri.strip_prefix(p)
            .is_some_and(|r| r.is_empty() || r.starts_with([':', '/']))
    });
    let https = uri
        .strip_prefix("https://")
        .is_some_and(|r| !r.is_empty() && !r.starts_with('/'));
    (https || local)
        && uri.len() <= 500
        && !uri.contains('#')
        && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn check_permissions(known: &Permissions, asked: &[String]) -> Result<Vec<String>> {
    if asked.is_empty() || asked.len() > 100 {
        return Err(invalid("an app asks for 1 to 100 permissions"));
    }
    if let Some(p) = asked.iter().find(|p| known.get(p).is_none()) {
        return Err(Error::unprocessable(format!("unknown permission {p:?}"))
            .with_code("unknown_permission"));
    }
    let mut out = asked.to_vec();
    out.sort();
    out.dedup();
    Ok(out)
}

/// An app's columns, its secret's hash and its owner.
type AppRow = (
    String,
    String,
    Vec<String>,
    Vec<String>,
    String,
    String,
    String,
);

async fn app_by_id(db: &Db, id: &str) -> Result<Option<(AppView, String, String)>> {
    if AppId::parse(id).is_err() {
        return Ok(None);
    }
    let row: Option<AppRow> = sqlx::query_as(&format!(
        "SELECT {APP_COLUMNS}, secret_hash, owner_id FROM apps WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&**db)
    .await?;
    Ok(row.map(
        |(id, name, redirect_uris, permissions, created_at, secret_hash, owner)| {
            (
                AppView {
                    id,
                    name,
                    redirect_uris,
                    permissions,
                    created_at,
                },
                secret_hash,
                owner,
            )
        },
    ))
}

/// Register an app (any signed-in person). Its `client_secret` is shown
/// once; its id is the `client_id`.
#[post("/api/apps", tag = "apps", status = 201)]
async fn register_app(
    db: Db,
    staff: Staff,
    known: State<Permissions>,
    Valid(Json(b)): Valid<Json<NewApp>>,
) -> Result<(StatusCode, Json<RegisteredApp>)> {
    if b.redirect_uris.is_empty() || b.redirect_uris.len() > 5 {
        return Err(invalid("an app has 1 to 5 redirect addresses"));
    }
    if let Some(bad) = b.redirect_uris.iter().find(|u| !is_redirect(u)) {
        return Err(invalid(format!(
            "{bad} is not a redirect address: https://…, or http://localhost while developing"
        )));
    }
    let permissions = check_permissions(&known, &b.permissions)?;
    let name = b.name.trim();
    if name.is_empty() {
        return Err(invalid("name cannot be blank"));
    }
    let id = AppId::new();
    let secret = format!("soat_cs_{}", new_token()?);
    let app = sqlx::query_as::<_, AppView>(&format!(
        "INSERT INTO apps (id, owner_id, name, secret_hash, redirect_uris, permissions) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {APP_COLUMNS}"
    ))
    .bind(id.as_str())
    .bind(staff.user_id.as_str())
    .bind(name)
    .bind(hash_token(&secret))
    .bind(&b.redirect_uris)
    .bind(&permissions)
    .fetch_one(&*db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(RegisteredApp {
            app,
            client_secret: secret,
        }),
    ))
}

/// Your own apps (any signed-in person).
#[get("/api/apps", tag = "apps")]
async fn list_apps(db: Db, staff: Staff) -> Result<Json<Vec<AppView>>> {
    let apps = sqlx::query_as::<_, AppView>(&format!(
        "SELECT {APP_COLUMNS} FROM apps WHERE owner_id = $1 ORDER BY id"
    ))
    .bind(staff.user_id.as_str())
    .fetch_all(&*db)
    .await?;
    Ok(Json(apps))
}

/// A new `client_secret` for one of your apps; the old one stops working.
#[post("/api/apps/{id}/secret", tag = "apps")]
async fn rotate_secret(
    db: Db,
    staff: Staff,
    Path(id): Path<String>,
) -> Result<Json<RegisteredApp>> {
    let Some((app, _, owner)) = app_by_id(&db, &id).await? else {
        return Err(Error::not_found());
    };
    if owner != staff.user_id.as_str() {
        return Err(Error::not_found());
    }
    let secret = format!("soat_cs_{}", new_token()?);
    sqlx::query("UPDATE apps SET secret_hash = $2 WHERE id = $1")
        .bind(&app.id)
        .bind(hash_token(&secret))
        .execute(&*db)
        .await?;
    Ok(Json(RegisteredApp {
        app,
        client_secret: secret,
    }))
}

async fn installation(conn: &mut PgConnection, id: &str) -> Result<Option<Installation>> {
    Ok(sqlx::query_as::<_, Installation>(&format!(
        "SELECT {INSTALL_COLUMNS} FROM app_installations i JOIN apps a ON a.id = i.app_id \
         WHERE i.id = $1"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

/// What installing app `client_id` would grant, for the consent screen
/// (`apps:manage`).
#[get("/api/app-consent/{client_id}", tag = "apps")]
async fn consent(
    db: Db,
    tenant: Tenant,
    known: State<Permissions>,
    Path(client_id): Path<String>,
) -> Result<Json<Consent>> {
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&APPS_MANAGE)?;
    let Some((app, _, _)) = app_by_id(&db, &client_id).await? else {
        return Err(Error::not_found());
    };
    let installed: Option<(String,)> =
        sqlx::query_as("SELECT id FROM app_installations WHERE tenant_id = $1 AND app_id = $2")
            .bind(tenant.id())
            .bind(&app.id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    let permissions = app
        .permissions
        .iter()
        .map(|p| AskedPermission {
            name: p.clone(),
            description: known
                .get(p)
                .map(|k| k.description.to_owned())
                .unwrap_or_default(),
            held: access.allows(p),
        })
        .collect();
    Ok(Json(Consent {
        client_id: app.id,
        name: app.name,
        permissions,
        installed: installed.is_some(),
    }))
}

/// Install an app with every permission it asks for (`apps:manage`,
/// signed-in people only, who must hold each of them). Installing again
/// renews the install, and the app's old key stops working once it trades
/// the new code. Publishes `app.installed`.
#[post("/api/app-installations", tag = "apps", status = 201)]
async fn install(
    db: Db,
    tenant: Tenant,
    Valid(Json(b)): Valid<Json<Install>>,
) -> Result<(StatusCode, Json<Installed>)> {
    let Some(staff) = tenant.staff().cloned() else {
        return Err(Error::forbidden().with_code("people_only"));
    };
    let Some((app, _, _)) = app_by_id(&db, &b.client_id).await? else {
        return Err(Error::not_found());
    };
    if !app.redirect_uris.contains(&b.redirect_uri) {
        return Err(
            Error::unprocessable("the redirect address is not one of the app's")
                .with_code("invalid_redirect"),
        );
    }
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&APPS_MANAGE)?;
    let beyond: Vec<&str> = app
        .permissions
        .iter()
        .filter(|p| !access.allows(p))
        .map(String::as_str)
        .collect();
    if !beyond.is_empty() {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            format!(
                "the app asks for permissions you lack: {}",
                beyond.join(", ")
            ),
        )
        .with_code("beyond_own_permissions"));
    }
    let id: (String,) = sqlx::query_as(
        "INSERT INTO app_installations (id, tenant_id, app_id, permissions, installed_by) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (tenant_id, app_id) DO UPDATE SET permissions = EXCLUDED.permissions, \
         installed_by = EXCLUDED.installed_by RETURNING id",
    )
    .bind(InstallationId::new().as_str())
    .bind(tenant.id())
    .bind(&app.id)
    .bind(&app.permissions)
    .bind(staff.user_id.as_str())
    .fetch_one(&mut *tx)
    .await?;
    let code = new_token()?;
    sqlx::query(
        "INSERT INTO app_codes (code_hash, tenant_id, installation_id, redirect_uri, expires_at) \
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5))",
    )
    .bind(hash_token(&code))
    .bind(tenant.id())
    .bind(&id.0)
    .bind(&b.redirect_uri)
    .bind(CODE_MINUTES)
    .execute(&mut *tx)
    .await?;
    let installation = installation(&mut tx, &id.0)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant
        .publish(&mut tx, "app.installed", &installation)
        .await?;
    tx.commit().await?;
    let mut query = vec![("code", code), ("shop", tenant.id().to_owned())];
    if let Some(state) = b.state.filter(|s| !s.is_empty()) {
        query.push(("state", state));
    }
    let sep = if b.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let redirect_to = format!(
        "{}{sep}{}",
        b.redirect_uri,
        serde_urlencoded::to_string(&query).unwrap_or_default()
    );
    Ok((
        StatusCode::CREATED,
        Json(Installed {
            installation,
            redirect_to,
        }),
    ))
}

/// The shop's installed apps (`apps:manage`).
#[get("/api/app-installations", tag = "apps")]
async fn list_installations(db: Db, tenant: Tenant) -> Result<Json<Vec<Installation>>> {
    let mut tx = tenant.begin_for(&db, &APPS_MANAGE).await?;
    let list = sqlx::query_as::<_, Installation>(&format!(
        "SELECT {INSTALL_COLUMNS} FROM app_installations i JOIN apps a ON a.id = i.app_id \
         WHERE i.tenant_id = $1 ORDER BY a.name"
    ))
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(list))
}

/// Uninstall an app (`apps:manage`): its key stops working at once.
/// Publishes `app.uninstalled`.
#[delete("/api/app-installations/{id}", tag = "apps", status = 204)]
async fn uninstall(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &APPS_MANAGE).await?;
    let found = match InstallationId::parse(&id) {
        Ok(_) => installation(&mut tx, &id).await?,
        Err(_) => None,
    }
    .ok_or_else(Error::not_found)?;
    let key: Option<(Option<String>,)> =
        sqlx::query_as("DELETE FROM app_installations WHERE id = $1 RETURNING api_key_id")
            .bind(&found.id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((Some(key),)) = key {
        revoke(&mut tx, &key).await?;
    }
    tenant
        .publish(
            &mut tx,
            "app.uninstalled",
            &serde_json::json!({ "id": found.id, "app_id": found.app_id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// App, permissions, who consented, and the install's current key.
type InstallRow = (String, Vec<String>, Option<String>, Option<String>);

fn bad_grant() -> Error {
    Error::bad_request("the code, app or secret is wrong or the code has expired")
        .with_code("invalid_grant")
}

/// Trade a consent's one-time code for the shop's key (the app, with its
/// `client_secret`; no sign-in). A code works once, for 10 minutes, with
/// the redirect address it was sent to; anything wrong is a 400
/// `invalid_grant`. A previous key of the install stops working.
/// Publishes `api_key.created`, without the key.
#[post("/api/apps/token", tag = "apps")]
async fn token(db: Db, Json(b): Json<TokenRequest>) -> Result<Json<AppToken>> {
    let Some((app, secret_hash, _)) = app_by_id(&db, &b.client_id).await? else {
        return Err(bad_grant());
    };
    if hash_token(&b.client_secret) != secret_hash {
        return Err(bad_grant());
    }
    let shop = Shop {
        id: TenantId::parse(&b.shop).map_err(|_| bad_grant())?,
    };
    let mut tx = shop.begin(&db).await.map_err(|_| bad_grant())?;
    // A code works once: it is gone whether or not it was still fresh.
    let row: Option<(String, String, bool)> = sqlx::query_as(
        "DELETE FROM app_codes WHERE code_hash = $1 AND tenant_id = $2 RETURNING \
         installation_id, redirect_uri, expires_at > now()",
    )
    .bind(hash_token(&b.code))
    .bind(shop.id())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((installation_id, redirect_uri, true)) = row else {
        tx.commit().await?;
        return Err(bad_grant());
    };
    let found: Option<InstallRow> = sqlx::query_as(
        "SELECT app_id, permissions, installed_by, api_key_id FROM app_installations \
         WHERE id = $1",
    )
    .bind(&installation_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((app_id, permissions, installed_by, old_key)) = found else {
        return Err(bad_grant());
    };
    if app_id != app.id || redirect_uri != b.redirect_uri {
        tx.commit().await?;
        return Err(bad_grant());
    }
    if let Some(old) = old_key {
        revoke(&mut tx, &old).await?;
    }
    let (key, secret) = insert_key(
        &mut tx,
        shop.id(),
        &format!("App: {}", app.name),
        permissions,
        installed_by.as_deref(),
    )
    .await?;
    sqlx::query("UPDATE app_installations SET api_key_id = $2 WHERE id = $1")
        .bind(&installation_id)
        .bind(&key.id)
        .execute(&mut *tx)
        .await?;
    shop.publish(&mut tx, None, "api_key.created", &key).await?;
    tx.commit().await?;
    Ok(Json(AppToken {
        access_token: secret,
        shop: shop.id().to_owned(),
        permissions: key.permissions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_are_https_or_local() {
        assert!(is_redirect("https://app.example.com/callback"));
        assert!(is_redirect("http://localhost:3000/cb"));
        assert!(is_redirect("http://127.0.0.1/cb"));
        for bad in [
            "http://app.example.com/cb",
            "https:///cb",
            "http://localhost.evil.com/cb",
            "https://app.example.com/cb#x",
            "javascript:alert(1)",
            "https://a b",
        ] {
            assert!(!is_redirect(bad), "{bad}");
        }
    }
}
