//! Tax providers: a service that works out the tax instead of the shop's
//! zones, for shops that sell where the rules go past a rate per country
//! (US sales tax by address, many registrations, an accountant's own
//! service).
//!
//! An adapter implements [`TaxProvider`] and is registered on the module
//! ([`crate::Taxes::with`]). The built-in `http` adapter calls any service
//! that answers SOAT's tax request (see `docs/tax-providers.md`). A shop
//! picks one with its credentials, sealed with the install's secret key
//! and never shown again. [`crate::quote`] asks it and keeps its answer
//! for a few minutes; when it cannot be reached, the zones' tax is used
//! and the quote says why.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use soat_core::secret::Sealer;
use soat_http::{Bytes, Full, Method, Request};
use soat_tenancy::{Scope, Tenant};
use zapline::auth::password::hash_token;
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{TAX_MANAGE, TAX_READ};

/// What an adapter call returns.
pub type TaxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// What a provider is asked.
#[derive(Debug, Clone)]
pub struct TaxRequest<'a> {
    /// The shop's credentials for it, unsealed.
    pub credentials: &'a Value,
    /// The shop's settings for it.
    pub settings: &'a Value,
    pub origin_country: Option<&'a str>,
    /// Where the goods go.
    pub country: &'a str,
    pub postal_code: Option<&'a str>,
    pub vat_number: Option<&'a str>,
    pub prices_include_tax: bool,
    /// `(tax_class, amount_minor)`, as [`crate::QuoteInput`] has them.
    pub lines: &'a [(String, i64)],
}

/// A provider's tax on one line.
#[model]
#[derive(Copy, PartialEq)]
pub struct ProviderLine {
    /// Basis points: 1900 is 19%.
    pub rate_bps: i64,
    pub tax_minor: i64,
}

/// A service that works out tax.
pub trait TaxProvider: Send + Sync {
    /// e.g. `http`.
    fn name(&self) -> &'static str;
    /// e.g. `Tax service (HTTP)`.
    fn label(&self) -> &'static str;
    /// Checks a shop's credentials and settings before they are kept.
    fn check(&self, credentials: &Value, settings: &Value) -> Result<()>;
    /// The tax on each line, in order.
    fn quote<'a>(&'a self, req: &'a TaxRequest<'a>) -> TaxFuture<'a, Vec<ProviderLine>>;
}

/// The adapters of this install and the key that seals credentials; an
/// app state that [`crate::quote`] takes.
#[derive(Clone)]
pub struct TaxProviders {
    list: Arc<Vec<Arc<dyn TaxProvider>>>,
    sealer: Option<Sealer>,
}

impl TaxProviders {
    pub(crate) fn new(list: Vec<Arc<dyn TaxProvider>>, sealer: Option<Sealer>) -> Self {
        Self {
            list: Arc::new(list),
            sealer,
        }
    }

    /// No providers: the zones only. For callers outside a request.
    pub fn none() -> Self {
        Self::new(Vec::new(), None)
    }

    /// The adapter called `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn TaxProvider>> {
        self.list.iter().find(|p| p.name() == name)
    }

    fn sealer(&self) -> Result<&Sealer> {
        self.sealer.as_ref().ok_or_else(|| {
            Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "this install keeps no secrets: set SOAT_SECRET_KEY",
            )
            .with_code("secrets_unavailable")
        })
    }
}

/// How long a provider's answer is kept.
const CACHE_MINUTES: i32 = 10;

/// What [`crate::quote`] got from the shop's provider.
pub(crate) enum Asked {
    /// The shop has none.
    NoProvider,
    /// It answered: its name and the lines.
    Answered(String, Vec<ProviderLine>),
    /// It failed: its name and why.
    Failed(String, String),
}

/// Asks the shop's provider, if it has one; a fresh enough earlier answer
/// to the same question is used again.
pub(crate) async fn ask(
    conn: &mut PgConnection,
    scope: &impl Scope,
    providers: &TaxProviders,
    req: TaxRequest<'_>,
) -> Result<Asked> {
    let row: Option<(String, SqlJson<Value>, Vec<u8>)> = sqlx::query_as(
        "SELECT provider, settings, credentials FROM tax_provider_accounts WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    let Some((name, settings, sealed)) = row else {
        return Ok(Asked::NoProvider);
    };
    let Some(provider) = providers.get(&name) else {
        return Ok(Asked::Failed(
            name,
            "this install has no such tax provider".into(),
        ));
    };
    let credentials = providers
        .sealer
        .as_ref()
        .and_then(|s| s.open(&sealed, scope.tenant_id()))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(credentials) = credentials else {
        return Ok(Asked::Failed(
            name,
            "the credentials cannot be opened".into(),
        ));
    };
    let question = json!({
        "provider": name, "settings": settings.0, "origin_country": req.origin_country,
        "country": req.country, "postal_code": req.postal_code, "vat_number": req.vat_number,
        "prices_include_tax": req.prices_include_tax, "lines": req.lines,
    });
    let key = hash_token(&question.to_string());
    let cached: Option<(SqlJson<Vec<ProviderLine>>,)> = sqlx::query_as(
        "SELECT lines FROM tax_quote_cache WHERE tenant_id = $1 AND key = $2 \
         AND expires_at > now()",
    )
    .bind(scope.tenant_id())
    .bind(&key)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((lines,)) = cached {
        return Ok(Asked::Answered(name, lines.0));
    }
    let req = TaxRequest {
        credentials: &credentials,
        settings: &settings.0,
        ..req
    };
    let lines = match provider.quote(&req).await {
        Ok(lines) if lines.len() == req.lines.len() => lines,
        Ok(_) => {
            return Ok(Asked::Failed(
                name,
                "the provider answered for a different number of lines".into(),
            ))
        }
        Err(e) => return Ok(Asked::Failed(name, e.to_string())),
    };
    sqlx::query(
        "INSERT INTO tax_quote_cache (tenant_id, key, lines, expires_at) \
         VALUES ($1, $2, $3, now() + make_interval(mins => $4)) \
         ON CONFLICT (tenant_id, key) DO UPDATE SET lines = EXCLUDED.lines, \
         expires_at = EXCLUDED.expires_at",
    )
    .bind(scope.tenant_id())
    .bind(&key)
    .bind(SqlJson(&lines))
    .bind(CACHE_MINUTES)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM tax_quote_cache WHERE tenant_id = $1 AND expires_at <= now()")
        .bind(scope.tenant_id())
        .execute(&mut *conn)
        .await?;
    Ok(Asked::Answered(name, lines))
}

/// The built-in adapter: POSTs SOAT's tax request as JSON to the shop's
/// `url`, signed like webhooks (`soat-signature`) with its `secret`.
pub struct HttpTaxProvider;

/// `https://…`, or plain HTTP on this machine for tests and development.
fn is_service_url(url: &str) -> bool {
    let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
        url.strip_prefix(p)
            .is_some_and(|r| r.is_empty() || r.starts_with([':', '/']))
    });
    let https = url
        .strip_prefix("https://")
        .is_some_and(|r| !r.is_empty() && !r.starts_with('/'));
    (https || local) && url.len() <= 500 && !url.chars().any(char::is_whitespace)
}

impl TaxProvider for HttpTaxProvider {
    fn name(&self) -> &'static str {
        "http"
    }

    fn label(&self) -> &'static str {
        "Tax service (HTTP)"
    }

    fn check(&self, credentials: &Value, settings: &Value) -> Result<()> {
        let url = settings["url"].as_str().unwrap_or_default();
        if !is_service_url(url) {
            return Err(
                Error::unprocessable("settings.url is the service's address: https://…")
                    .with_code("invalid_settings"),
            );
        }
        let secret = credentials["secret"].as_str().unwrap_or_default();
        if secret.len() < 16 {
            return Err(Error::unprocessable(
                "credentials.secret signs the requests: at least 16 characters",
            )
            .with_code("invalid_credentials"));
        }
        Ok(())
    }

    fn quote<'a>(&'a self, req: &'a TaxRequest<'a>) -> TaxFuture<'a, Vec<ProviderLine>> {
        Box::pin(async move {
            let url = req.settings["url"].as_str().unwrap_or_default();
            let secret = req.credentials["secret"].as_str().unwrap_or_default();
            let lines: Vec<Value> = req
                .lines
                .iter()
                .map(|(class, amount)| json!({ "tax_class": class, "amount_minor": amount }))
                .collect();
            let body = json!({
                "origin_country": req.origin_country, "country": req.country,
                "postal_code": req.postal_code, "vat_number": req.vat_number,
                "prices_include_tax": req.prices_include_tax, "lines": lines,
            })
            .to_string();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let signature = soat_webhooks::sign(secret, now, body.as_bytes());
            let request = Request::builder()
                .method(Method::POST)
                .uri(url)
                .header("content-type", "application/json")
                .header("soat-signature", signature)
                .body(Full::new(Bytes::from(body)))
                .map_err(|e| Error::unprocessable(e.to_string()))?;
            let answer = soat_http::send("Tax service", request).await?;
            if !answer.status.is_success() {
                return Err(Error::service_unavailable(format!(
                    "Tax service: answered {}",
                    answer.status.as_u16()
                )));
            }
            let lines = answer.json()["lines"].clone();
            serde_json::from_value::<Vec<ProviderLine>>(lines)
                .map_err(|_| Error::service_unavailable("Tax service: the answer has no lines"))
        })
    }
}

/// An adapter this install offers.
#[model]
pub struct ProviderKind {
    pub name: String,
    pub label: String,
}

/// The shop's tax provider.
#[model]
pub struct ShopTaxProvider {
    /// The adapter, e.g. `http`.
    pub provider: String,
    /// Its settings, e.g. `{"url": "https://tax.example.com/quote"}`.
    pub settings: Value,
    pub created_at: String,
}

/// Body of `PUT /api/tax/provider`.
#[model]
pub struct SetTaxProvider {
    pub provider: String,
    #[serde(default)]
    pub settings: Value,
    /// Kept sealed, never shown again; for `http`, `{"secret": "…"}`.
    pub credentials: Value,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((list_kinds, get_provider, put_provider, delete_provider))
}

/// The tax providers this install offers (`tax:read`).
#[get("/api/tax/providers", tag = "tax")]
async fn list_kinds(
    db: Db,
    tenant: Tenant,
    providers: State<TaxProviders>,
) -> Result<Json<Vec<ProviderKind>>> {
    let tx = tenant.begin_for(&db, &TAX_READ).await?;
    tx.commit().await?;
    Ok(Json(
        providers
            .list
            .iter()
            .map(|p| ProviderKind {
                name: p.name().to_owned(),
                label: p.label().to_owned(),
            })
            .collect(),
    ))
}

async fn current(conn: &mut PgConnection, tenant: &Tenant) -> Result<Option<ShopTaxProvider>> {
    let row: Option<(String, SqlJson<Value>, String)> = sqlx::query_as(
        "SELECT provider, settings, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM tax_provider_accounts WHERE tenant_id = $1",
    )
    .bind(tenant.id())
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(provider, settings, created_at)| ShopTaxProvider {
        provider,
        settings: settings.0,
        created_at,
    }))
}

/// The shop's tax provider (`tax:read`); 404 when the zones work out tax.
#[get("/api/tax/provider", tag = "tax")]
async fn get_provider(db: Db, tenant: Tenant) -> Result<Json<ShopTaxProvider>> {
    let mut tx = tenant.begin_for(&db, &TAX_READ).await?;
    let found = current(&mut tx, &tenant).await?;
    tx.commit().await?;
    found.map(Json).ok_or_else(Error::not_found)
}

/// Let a provider work out the shop's tax (`tax:manage`), replacing any
/// before. The credentials are sealed and never shown again. Publishes
/// `tax.provider_set`, without them.
#[put("/api/tax/provider", tag = "tax")]
async fn put_provider(
    db: Db,
    tenant: Tenant,
    providers: State<TaxProviders>,
    Json(b): Json<SetTaxProvider>,
) -> Result<Json<ShopTaxProvider>> {
    let adapter = providers.get(&b.provider).ok_or_else(|| {
        Error::unprocessable(format!("no tax provider {:?}", b.provider))
            .with_code("unknown_provider")
    })?;
    let settings = if b.settings.is_null() {
        json!({})
    } else {
        b.settings
    };
    adapter.check(&b.credentials, &settings)?;
    let sealed = providers
        .sealer()?
        .seal(b.credentials.to_string().as_bytes(), tenant.id());
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    sqlx::query(
        "INSERT INTO tax_provider_accounts (tenant_id, provider, settings, credentials) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id) DO UPDATE SET \
         provider = EXCLUDED.provider, settings = EXCLUDED.settings, \
         credentials = EXCLUDED.credentials, created_at = now()",
    )
    .bind(tenant.id())
    .bind(&b.provider)
    .bind(SqlJson(&settings))
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM tax_quote_cache WHERE tenant_id = $1")
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    let saved = current(&mut tx, &tenant)
        .await?
        .ok_or_else(Error::not_found)?;
    tenant.publish(&mut tx, "tax.provider_set", &saved).await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// Go back to the shop's zones (`tax:manage`). Publishes
/// `tax.provider_removed`.
#[delete("/api/tax/provider", tag = "tax", status = 204)]
async fn delete_provider(db: Db, tenant: Tenant) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let gone: Option<(String,)> =
        sqlx::query_as("DELETE FROM tax_provider_accounts WHERE tenant_id = $1 RETURNING provider")
            .bind(tenant.id())
            .fetch_optional(&mut *tx)
            .await?;
    let Some((provider,)) = gone else {
        return Err(Error::not_found());
    };
    sqlx::query("DELETE FROM tax_quote_cache WHERE tenant_id = $1")
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "tax.provider_removed",
            &json!({ "provider": provider }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_http_adapter_checks_its_settings() {
        let p = HttpTaxProvider;
        let secret = json!({ "secret": "a secret of some length" });
        assert!(p
            .check(&secret, &json!({ "url": "https://tax.example.com/q" }))
            .is_ok());
        assert!(p
            .check(&secret, &json!({ "url": "http://127.0.0.1:9/q" }))
            .is_ok());
        for url in ["http://tax.example.com/q", "ftp://x", "", "https:///q"] {
            assert!(p.check(&secret, &json!({ "url": url })).is_err(), "{url}");
        }
        assert!(p
            .check(
                &json!({ "secret": "short" }),
                &json!({ "url": "https://tax.example.com/q" })
            )
            .is_err());
    }
}
