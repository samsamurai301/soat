//! Moving to SOAT from Shopify without a gap: a shop connects its Shopify
//! store (its `*.myshopify.com` domain and an Admin API access token from
//! a custom app with read access to products and customers), then pulls
//! what changed since the last sync as often as it likes while both shops
//! run. Each pull returns SOAT's own import file, which the admin imports
//! through the API like an uploaded one, then records as synced. Cutover
//! is the last sync: after it, pulls are refused.
//!
//! Only the shop's own domain is ever called (or `SOAT_SHOPIFY_API_BASE`,
//! a local fake in tests), and the token is sealed with the install's key.

use serde_json::{json, Value};
use soat_core::csv::{write, Table};
use soat_core::secret::Sealer;
use soat_core::{shopify, Permission};
use soat_http::{send, Bytes, Full, Method, Request};
use soat_module::Module;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

/// Connect a Shopify store, pull from it and cut over.
pub const IMPORTS_MANAGE: Permission = Permission::new(
    "imports:manage",
    "Connect a Shopify store, pull its changes and cut over",
);

/// The Admin API version SOAT reads.
pub const API_VERSION: &str = "2025-01";
/// Items per page, Shopify's most.
const PAGE: usize = 250;
/// Pages per pull: 10 000 items; a pull that hits it says so.
const MAX_PAGES: usize = 40;

/// The import module.
#[derive(Clone, Default)]
pub struct Imports {
    sealer: Option<Sealer>,
    api_base: Option<String>,
}

impl Imports {
    pub fn new() -> Self {
        Self::default()
    }

    /// The key that seals access tokens.
    #[must_use]
    pub fn with_sealer(mut self, sealer: Option<Sealer>) -> Self {
        self.sealer = sealer;
        self
    }

    /// Call `base` instead of the shop's own domain (a local fake in
    /// tests); empty keeps the domain.
    #[must_use]
    pub fn with_api_base(mut self, base: Option<String>) -> Self {
        self.api_base = base
            .filter(|b| !b.trim().is_empty())
            .map(|b| b.trim_end_matches('/').to_owned());
        self
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

    fn base(&self, domain: &str) -> String {
        self.api_base
            .clone()
            .unwrap_or_else(|| format!("https://{domain}"))
    }
}

impl Module for Imports {
    fn name(&self) -> &'static str {
        "import"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[IMPORTS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.state(self.clone()).mount((
            get_shopify,
            connect_shopify,
            disconnect_shopify,
            pull_shopify,
            synced_shopify,
            cut_over_shopify,
        ))
    }
}

/// A shop's connection to its Shopify store. The token is never shown.
#[model]
#[derive(sqlx::FromRow)]
pub struct ShopifyImport {
    /// e.g. `acme.myshopify.com`.
    pub shop_domain: String,
    /// Until when products are synced, RFC 3339; null before the first.
    pub products_synced_at: Option<String>,
    pub customers_synced_at: Option<String>,
    /// When the shop cut over; no more pulls after it.
    pub cut_over_at: Option<String>,
    pub created_at: String,
}

/// Body of `PUT /api/imports/shopify`.
#[model]
pub struct ConnectShopify {
    /// The store's `*.myshopify.com` domain.
    #[validate(length(min = 1, max = 100))]
    pub shop_domain: String,
    /// An Admin API access token (`shpat_...`) with read access to products
    /// and customers.
    #[validate(length(min = 1, max = 200))]
    pub access_token: String,
}

/// Body of a pull or of recording a sync.
#[model]
pub struct PullShopify {
    /// `products` or `customers`.
    pub what: String,
}

/// What a pull found, as SOAT's import file.
#[model]
pub struct Pulled {
    pub what: String,
    /// SOAT's product or customer file, CSV.
    pub csv: String,
    /// How many products or customers changed.
    pub count: usize,
    /// Record it with `POST /api/imports/shopify/synced` once imported.
    pub until: String,
    /// More changed than one pull takes: pull again after recording.
    pub more: bool,
}

/// Body of `POST /api/imports/shopify/synced`.
#[model]
pub struct Synced {
    /// `products` or `customers`.
    pub what: String,
    /// The `until` of the pull that was imported.
    #[validate(length(min = 1, max = 40))]
    pub until: String,
}

const COLUMNS: &str = "shop_domain, \
     to_char(products_synced_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS products_synced_at, \
     to_char(customers_synced_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS customers_synced_at, \
     to_char(cut_over_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS cut_over_at, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

/// `acme.myshopify.com`: the only hosts SOAT calls for a shop.
fn is_shop_domain(d: &str) -> bool {
    d.strip_suffix(".myshopify.com").is_some_and(|name| {
        (1..=60).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
    })
}

fn what(w: &str) -> Result<&'static str> {
    match w {
        "products" => Ok("products"),
        "customers" => Ok("customers"),
        _ => Err(Error::unprocessable("what is products or customers").with_code("invalid_import")),
    }
}

async fn load(conn: &mut PgConnection, scope: &impl Scope) -> Result<Option<ShopifyImport>> {
    Ok(sqlx::query_as::<_, ShopifyImport>(&format!(
        "SELECT {COLUMNS} FROM shopify_imports WHERE tenant_id = $1"
    ))
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?)
}

/// Calls Shopify's Admin API at `path` (after `/admin/api/<version>`).
async fn call(imports: &Imports, domain: &str, token: &str, path: &str) -> Result<Value> {
    let unavailable = |e: String| Error::service_unavailable(format!("Shopify: {e}"));
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "{}/admin/api/{API_VERSION}{path}",
            imports.base(domain)
        ))
        .header("x-shopify-access-token", token)
        .header("accept", "application/json")
        .header("user-agent", "SOAT/1")
        .body(Full::new(Bytes::new()))
        .map_err(|e| unavailable(e.to_string()))?;
    let answer = send("Shopify", req).await?;
    match answer.status.as_u16() {
        200..=299 => Ok(answer.json()),
        401 | 403 => Err(Error::unprocessable(
            "Shopify refused the access token, or it lacks read access to products and customers",
        )
        .with_code("invalid_credentials")),
        404 => Err(Error::unprocessable("Shopify knows no such store").with_code("unknown_store")),
        s if s >= 500 || s == 429 => Err(unavailable(format!("answered {s}; try again"))),
        s => Err(Error::unprocessable(format!("Shopify answered {s}")).with_code("shopify_error")),
    }
}

/// The shop's connection to its Shopify store (`imports:manage`); 404
/// before one is made.
#[get("/api/imports/shopify", tag = "imports")]
async fn get_shopify(db: Db, tenant: Tenant) -> Result<Json<ShopifyImport>> {
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    let found = load(&mut tx, &tenant).await?;
    tx.commit().await?;
    found.map(Json).ok_or_else(Error::not_found)
}

/// Connect the shop's Shopify store, or change its token
/// (`imports:manage`). The token is tried on Shopify first (422
/// `invalid_credentials` when refused), then sealed and never shown again.
/// Connecting anew starts syncing from the beginning. Publishes
/// `shopify_import.connected`, without the token.
#[put("/api/imports/shopify", tag = "imports")]
async fn connect_shopify(
    db: Db,
    tenant: Tenant,
    imports: State<Imports>,
    Valid(Json(b)): Valid<Json<ConnectShopify>>,
) -> Result<Json<ShopifyImport>> {
    let domain = b.shop_domain.trim().to_ascii_lowercase();
    let domain = domain
        .strip_prefix("https://")
        .unwrap_or(&domain)
        .trim_end_matches('/')
        .to_owned();
    if !is_shop_domain(&domain) {
        return Err(
            Error::unprocessable("the store's domain ends in .myshopify.com")
                .with_code("invalid_shop_domain"),
        );
    }
    let token = b.access_token.trim();
    // Checked before a transaction is open: the call can take seconds.
    tenant
        .begin_for(&db, &IMPORTS_MANAGE)
        .await?
        .commit()
        .await?;
    call(&imports, &domain, token, "/shop.json").await?;
    let sealed = imports.sealer()?.seal(token.as_bytes(), tenant.id());
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    sqlx::query(
        "INSERT INTO shopify_imports (tenant_id, shop_domain, token) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id) DO UPDATE SET token = EXCLUDED.token, \
         shop_domain = EXCLUDED.shop_domain, \
         products_synced_at = CASE WHEN shopify_imports.shop_domain = EXCLUDED.shop_domain \
           THEN shopify_imports.products_synced_at END, \
         customers_synced_at = CASE WHEN shopify_imports.shop_domain = EXCLUDED.shop_domain \
           THEN shopify_imports.customers_synced_at END",
    )
    .bind(tenant.id())
    .bind(&domain)
    .bind(&sealed)
    .execute(&mut *tx)
    .await?;
    let found = load(&mut tx, &tenant).await?.ok_or_else(Error::not_found)?;
    tenant
        .publish(&mut tx, "shopify_import.connected", &found)
        .await?;
    tx.commit().await?;
    Ok(Json(found))
}

/// Forget the Shopify store and its token (`imports:manage`). Publishes
/// `shopify_import.disconnected`.
#[delete("/api/imports/shopify", tag = "imports", status = 204)]
async fn disconnect_shopify(db: Db, tenant: Tenant) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    let found = load(&mut tx, &tenant).await?.ok_or_else(Error::not_found)?;
    sqlx::query("DELETE FROM shopify_imports WHERE tenant_id = $1")
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "shopify_import.disconnected",
            &json!({ "shop_domain": found.shop_domain }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Domain, sealed token, since when, whether cut over, and now.
type PullRow = (String, Vec<u8>, Option<String>, bool, String);

/// The products or customers changed on Shopify since the last recorded
/// sync (a minute's overlap, so nothing slips between), as SOAT's import
/// file (`imports:manage`). Import it, then record it with
/// `POST /api/imports/shopify/synced`. 409 `cut_over` after cutover.
#[post("/api/imports/shopify/pull", tag = "imports")]
async fn pull_shopify(
    db: Db,
    tenant: Tenant,
    imports: State<Imports>,
    Json(b): Json<PullShopify>,
) -> Result<Json<Pulled>> {
    let what = what(&b.what)?;
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    let row: Option<PullRow> = sqlx::query_as(&format!(
        "SELECT shop_domain, token, \
         to_char(({what}_synced_at - interval '1 minute') AT TIME ZONE 'UTC', \
           'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
         cut_over_at IS NOT NULL, \
         to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM shopify_imports WHERE tenant_id = $1"
    ))
    .bind(tenant.id())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let (domain, sealed, since, cut_over, until) = row.ok_or_else(Error::not_found)?;
    if cut_over {
        return Err(Error::conflict("the shop has cut over from Shopify").with_code("cut_over"));
    }
    let token = imports
        .sealer()?
        .open(&sealed, tenant.id())
        .and_then(|t| String::from_utf8(t).ok())
        .ok_or_else(|| {
            Error::unprocessable("the token cannot be read: connect the store again")
                .with_code("invalid_credentials")
        })?;
    let mut items: Vec<Value> = Vec::new();
    let mut since_id = 0_i64;
    let mut more = false;
    for page in 0..MAX_PAGES {
        let mut query = format!("?limit={PAGE}&since_id={since_id}");
        if let Some(s) = &since {
            query.push_str(&format!("&updated_at_min={s}"));
        }
        let answer = call(&imports, &domain, &token, &format!("/{what}.json{query}")).await?;
        let batch = answer[what].as_array().cloned().unwrap_or_default();
        let full = batch.len() == PAGE;
        since_id = batch
            .iter()
            .filter_map(|i| i["id"].as_i64())
            .max()
            .unwrap_or(since_id);
        items.extend(batch);
        if !full {
            break;
        }
        more = page + 1 == MAX_PAGES;
    }
    let table: Table = match what {
        "products" => shopify::api_products(&items),
        _ => shopify::api_customers(&items),
    };
    let mut rows = vec![table.headers.clone()];
    rows.extend(table.rows);
    Ok(Json(Pulled {
        what: what.to_owned(),
        csv: write(&rows),
        count: items.len(),
        until,
        more,
    }))
}

/// Record that a pull's products or customers are imported, so the next
/// pull starts from its `until` (`imports:manage`). Publishes
/// `shopify_import.synced`.
#[post("/api/imports/shopify/synced", tag = "imports")]
async fn synced_shopify(
    db: Db,
    tenant: Tenant,
    Valid(Json(b)): Valid<Json<Synced>>,
) -> Result<Json<ShopifyImport>> {
    let what = what(&b.what)?;
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    let done = sqlx::query(&format!(
        "UPDATE shopify_imports SET {what}_synced_at = $2::timestamptz \
         WHERE tenant_id = $1 AND cut_over_at IS NULL"
    ))
    .bind(tenant.id())
    .bind(&b.until)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("22007") => {
            Error::unprocessable("until is an RFC 3339 time").with_code("invalid_import")
        }
        _ => e.into(),
    })?;
    if done.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    let found = load(&mut tx, &tenant).await?.ok_or_else(Error::not_found)?;
    tenant
        .publish(
            &mut tx,
            "shopify_import.synced",
            &json!({ "what": what, "until": b.until }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(found))
}

/// Cut over: the last sync is done and SOAT is the shop now
/// (`imports:manage`). Later pulls are a 409. Publishes
/// `shopify_import.cut_over`.
#[post("/api/imports/shopify/cutover", tag = "imports")]
async fn cut_over_shopify(db: Db, tenant: Tenant) -> Result<Json<ShopifyImport>> {
    let mut tx = tenant.begin_for(&db, &IMPORTS_MANAGE).await?;
    sqlx::query(
        "UPDATE shopify_imports SET cut_over_at = COALESCE(cut_over_at, now()) \
         WHERE tenant_id = $1",
    )
    .bind(tenant.id())
    .execute(&mut *tx)
    .await?;
    let found = load(&mut tx, &tenant).await?.ok_or_else(Error::not_found)?;
    tenant
        .publish(&mut tx, "shopify_import.cut_over", &found)
        .await?;
    tx.commit().await?;
    Ok(Json(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shopify_domains_are_called() {
        assert!(is_shop_domain("acme.myshopify.com"));
        assert!(is_shop_domain("acme-2.myshopify.com"));
        for bad in [
            "acme.com",
            "evil.com/x.myshopify.com",
            "a.b.myshopify.com",
            ".myshopify.com",
            "-a.myshopify.com",
            "127.0.0.1:80#.myshopify.com",
        ] {
            assert!(!is_shop_domain(bad), "{bad}");
        }
    }
}
