//! The cookie banner on the storefront (feature `privacy`): asked once
//! per banner version, with "Accept all" and "Reject all" side by side and
//! a choice per purpose; what needs consent loads only after it.

use serde_json::{json, Map, Value};
use soat_core::privacy::Consent;
use soat_tenancy::Shop;
use zapline::db::sqlx::PgConnection;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{cookie, put_cookie, root_of, View};

/// The cookie holding the visitor's choice.
pub const CONSENT_COOKIE: &str = "soat_consent";
/// How long a choice is kept before asking again: six months.
const KEEP_SECONDS: i64 = 182 * 86_400;

/// Puts the banner's state in `data` (`consent`), and takes out what the
/// visitor did not agree to: apps' scripts and blocks without `apps`.
/// Tracking codes are given only with consent to their purpose.
pub(crate) async fn apply(
    conn: &mut PgConnection,
    shop: &Shop,
    headers: &HeaderMap,
    data: &mut Map<String, Value>,
) -> Result<()> {
    let s = soat_compliance::consent::settings(conn, shop).await?;
    if !s.enabled {
        data.insert("consent".to_owned(), json!({ "enabled": false }));
        return Ok(());
    }
    let given = cookie(headers, CONSENT_COOKIE)
        .and_then(|c| Consent::parse(&c))
        .filter(|c| c.version == s.version);
    let c = given.unwrap_or_else(|| Consent::all(s.version, false));
    if !c.apps {
        data.remove("app_scripts");
        data.remove("app_blocks");
    }
    data.insert(
        "consent".to_owned(),
        json!({
            "enabled": true,
            "ask": given.is_none(),
            "analytics": c.analytics,
            "marketing": c.marketing,
            "apps": c.apps,
            "privacy_url": s.privacy_url,
            "ga4_id": Some(&s.ga4_id).filter(|g| c.analytics && !g.is_empty()),
            "meta_pixel_id": Some(&s.meta_pixel_id).filter(|p| c.marketing && !p.is_empty()),
        }),
    );
    Ok(())
}

/// The banner's form.
#[model]
pub struct ConsentForm {
    /// `all`, `none`, or `chosen` for the boxes below.
    pub choice: String,
    pub analytics: Option<String>,
    pub marketing: Option<String>,
    pub apps: Option<String>,
    /// The page to go back to.
    pub back: Option<String>,
}

/// Record a visitor's choice (a form post) and go back to their page.
#[post("/consent", tag = "storefront")]
pub(crate) async fn choose(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<ConsentForm>,
) -> Result<Response> {
    let (analytics, marketing, apps) = match f.choice.as_str() {
        "all" => (true, true, true),
        "none" => (false, false, false),
        _ => (
            f.analytics.is_some(),
            f.marketing.is_some(),
            f.apps.is_some(),
        ),
    };
    let mut tx = shop.begin(&db).await?;
    let given =
        soat_compliance::consent::record(&mut tx, &shop, analytics, marketing, apps).await?;
    tx.commit().await?;
    let back = f
        .back
        .filter(|b| {
            b.starts_with('/')
                && !b.starts_with("//")
                && !b.contains('\\')
                && !b.chars().any(char::is_control)
        })
        .unwrap_or_else(|| format!("{}/", root_of(&headers)));
    let mut res = Redirect::to(&back).into_response();
    put_cookie(
        &mut res,
        CONSENT_COOKIE,
        &given.cookie(),
        KEEP_SECONDS,
        conn.secure,
    );
    Ok(res)
}

/// The cookie settings, to change a choice at any time.
#[get("/consent", tag = "storefront")]
pub(crate) async fn page(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    v.set("page_title", json!(v.t("consent_settings")));
    v.render("consent", StatusCode::OK).await
}
