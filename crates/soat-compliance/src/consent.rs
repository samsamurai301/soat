//! The cookie banner: a shop asks visitors before it loads anything that
//! tracks them or comes from a third party (GDPR, ePrivacy, § 25 TDDDG).
//!
//! SOAT itself sets only cookies the shop needs to work (cart, sign-in,
//! order, country) and counts visits without cookies, so a shop without
//! third-party tracking and apps needs no banner. With the banner on:
//!
//! - Google Analytics 4 loads only after consent to `analytics`;
//! - the Meta Pixel loads only after consent to `marketing`;
//! - apps' storefront scripts load only after consent to `apps`.
//!
//! "Accept all" and "Reject all" are offered side by side. Each choice is
//! recorded without the visitor's identity, as proof (GDPR art. 7(1)).

use serde_json::json;
use soat_core::privacy::Consent;
use soat_core::{id_kind, Permission};
use soat_tenancy::{Scope, Shop, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::PRIVACY_READ;

id_kind!(ConsentRecordId, ConsentRecordKind, "cns");

/// Set up the cookie banner and the tracking it governs.
pub const PRIVACY_SETTINGS: Permission = Permission::new(
    "privacy:settings",
    "Set up the cookie banner and tracking codes",
);

/// The shop's cookie banner.
#[model]
#[derive(sqlx::FromRow, Default)]
pub struct ConsentSettings {
    /// Whether visitors are asked.
    pub enabled: bool,
    /// Bumped to ask everyone again.
    pub version: i64,
    /// The shop's privacy policy, linked from the banner.
    pub privacy_url: String,
    /// e.g. `G-XXXXXXXXXX`; loaded only with consent to analytics.
    pub ga4_id: String,
    /// e.g. `123456789012345`; loaded only with consent to marketing.
    pub meta_pixel_id: String,
}

/// The shop's banner settings; off with version 1 when never set.
///
/// # Errors
///
/// Database errors.
pub async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<ConsentSettings> {
    let row: Option<ConsentSettings> = sqlx::query_as(
        "SELECT enabled, version, privacy_url, ga4_id, meta_pixel_id FROM consent_settings \
         WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(conn)
    .await?;
    Ok(row.unwrap_or(ConsentSettings {
        version: 1,
        ..ConsentSettings::default()
    }))
}

/// Records a visitor's choice and gives it back with the banner's version,
/// for the consent cookie.
///
/// # Errors
///
/// Database errors.
pub async fn record(
    conn: &mut PgConnection,
    shop: &Shop,
    analytics: bool,
    marketing: bool,
    apps: bool,
) -> Result<Consent> {
    let s = settings(&mut *conn, shop).await?;
    let c = Consent {
        version: s.version,
        analytics,
        marketing,
        apps,
    };
    sqlx::query(
        "INSERT INTO consent_records (id, tenant_id, version, analytics, marketing, apps) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(ConsentRecordId::new().as_str())
    .bind(shop.id())
    .bind(c.version)
    .bind(c.analytics)
    .bind(c.marketing)
    .bind(c.apps)
    .execute(conn)
    .await?;
    Ok(c)
}

/// Changes to the banner.
#[model]
pub struct ConsentChange {
    pub enabled: Option<bool>,
    #[validate(length(max = 500))]
    pub privacy_url: Option<String>,
    #[validate(length(max = 40))]
    pub ga4_id: Option<String>,
    #[validate(length(max = 40))]
    pub meta_pixel_id: Option<String>,
    /// Ask every visitor again (after adding tracking, say).
    pub ask_again: Option<bool>,
}

fn checked(c: &ConsentChange) -> Result<()> {
    let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_consent"));
    if let Some(g) = c.ga4_id.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
        let ok =
            g.len() > 2 && g.starts_with("G-") && g[2..].bytes().all(|b| b.is_ascii_alphanumeric());
        if !ok {
            return bad("a Google Analytics 4 id looks like G-XXXXXXXXXX");
        }
    }
    if let Some(p) = c
        .meta_pixel_id
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return bad("a Meta Pixel id is digits only");
        }
    }
    if let Some(u) = c
        .privacy_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        if !(u.starts_with('/') || u.starts_with("https://")) {
            return bad("the privacy policy is a path like /pages/privacy or an https:// address");
        }
    }
    Ok(())
}

/// The shop's cookie banner (`privacy:read`).
#[get("/api/consent", tag = "privacy")]
pub(crate) async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<ConsentSettings>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Set up the cookie banner and the tracking it governs
/// (`privacy:settings`); `ask_again` makes a new version, so every visitor
/// is asked again. Publishes `consent.updated`.
#[put("/api/consent", tag = "privacy")]
pub(crate) async fn put_settings(
    db: Db,
    tenant: Tenant,
    Valid(Json(c)): Valid<Json<ConsentChange>>,
) -> Result<Json<ConsentSettings>> {
    checked(&c)?;
    let mut tx = tenant.begin_for(&db, &PRIVACY_SETTINGS).await?;
    let trim = |v: &Option<String>| v.as_deref().map(|s| s.trim().to_owned());
    sqlx::query(
        "INSERT INTO consent_settings (tenant_id, enabled, privacy_url, ga4_id, meta_pixel_id) \
         VALUES ($1, COALESCE($2, FALSE), COALESCE($3, ''), COALESCE($4, ''), COALESCE($5, '')) \
         ON CONFLICT (tenant_id) DO UPDATE SET \
         enabled = COALESCE($2, consent_settings.enabled), \
         privacy_url = COALESCE($3, consent_settings.privacy_url), \
         ga4_id = COALESCE($4, consent_settings.ga4_id), \
         meta_pixel_id = COALESCE($5, consent_settings.meta_pixel_id), \
         version = consent_settings.version + CASE WHEN $6 THEN 1 ELSE 0 END, \
         updated_at = now()",
    )
    .bind(tenant.id())
    .bind(c.enabled)
    .bind(trim(&c.privacy_url))
    .bind(trim(&c.ga4_id))
    .bind(trim(&c.meta_pixel_id))
    .bind(c.ask_again.unwrap_or(false))
    .execute(&mut *tx)
    .await?;
    let s = settings(&mut tx, &tenant).await?;
    tenant.publish(&mut tx, "consent.updated", &s).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// How visitors chose.
#[model]
#[derive(Default)]
pub struct ConsentStats {
    /// Choices in the last 30 days.
    pub choices: i64,
    pub accepted_all: i64,
    pub rejected_all: i64,
    pub analytics: i64,
    pub marketing: i64,
    pub apps: i64,
}

/// How visitors chose in the banner in the last 30 days (`privacy:read`).
#[get("/api/consent/stats", tag = "privacy")]
pub(crate) async fn stats(db: Db, tenant: Tenant) -> Result<Json<ConsentStats>> {
    let mut tx = tenant.begin_for(&db, &PRIVACY_READ).await?;
    let row: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), \
         count(*) FILTER (WHERE analytics AND marketing AND apps), \
         count(*) FILTER (WHERE NOT analytics AND NOT marketing AND NOT apps), \
         count(*) FILTER (WHERE analytics), count(*) FILTER (WHERE marketing), \
         count(*) FILTER (WHERE apps) \
         FROM consent_records WHERE tenant_id = $1 AND created_at > now() - interval '30 days'",
    )
    .bind(tenant.id())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(ConsentStats {
        choices: row.0,
        accepted_all: row.1,
        rejected_all: row.2,
        analytics: row.3,
        marketing: row.4,
        apps: row.5,
    }))
}

/// A choice made in a headless storefront's own banner.
#[model]
pub struct ConsentChoice {
    pub analytics: bool,
    pub marketing: bool,
    pub apps: bool,
}

/// The banner's settings, for a headless storefront.
#[get("/api/storefront/consent", tag = "storefront")]
pub(crate) async fn storefront_settings(db: Db, shop: Shop) -> Result<Json<serde_json::Value>> {
    let mut tx = shop.begin(&db).await?;
    let s = settings(&mut tx, &shop).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "enabled": s.enabled, "version": s.version, "privacy_url": s.privacy_url,
        "ga4_id": s.ga4_id, "meta_pixel_id": s.meta_pixel_id,
    })))
}

/// Record a visitor's choice made in a headless storefront's banner;
/// answers the value to keep in the `soat_consent` cookie.
#[post("/api/storefront/consent", tag = "storefront", status = 201)]
pub(crate) async fn storefront_record(
    db: Db,
    shop: Shop,
    Json(c): Json<ConsentChoice>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let mut tx = shop.begin(&db).await?;
    let given = record(&mut tx, &shop, c.analytics, c.marketing, c.apps).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "version": given.version, "cookie": given.cookie() })),
    ))
}
