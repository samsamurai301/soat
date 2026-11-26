//! The storefront as JSON, for a shop whose front end is built elsewhere (a
//! Next.js, Astro or mobile app): the catalog, content pages and menus in
//! the buyer's language and market. Carts, checkout and customer accounts
//! have their own `/api/storefront/...` endpoints.
//!
//! Every endpoint is public. The shop comes from the `x-soat-tenant`
//! header (or the `Host`), the language from `?lang=de` (or a `/de/`
//! prefix), the market from `?country=CH`. A language the shop does not
//! publish falls back to its default; a country outside its markets
//! keeps the shop's own prices.

use soat_catalog::storefront::{self as catalog, ProductCard, ShopCollection, ShopProduct};
use soat_cms::{Menu, Page};
use soat_i18n::shop_languages;
use soat_tenancy::{Scope, Shop, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::middleware::{Cors, Middleware};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::i18n::{self, Lang, LANGUAGE_HEADER};

/// Any web page may call the storefront API from its browser: it answers
/// public data, or data a bearer token or a cart's id opens, and cookies
/// are never sent along (no credentials). The admin API stays
/// same-origin.
static CORS: std::sync::LazyLock<Cors> = std::sync::LazyLock::new(|| {
    Cors::permissive().allow_headers(
        "authorization, content-type, accept, x-soat-tenant, x-soat-cart-token, x-soat-order-token",
    )
});

/// Whether `path` is the storefront API, in any language.
fn is_api(path: &str) -> bool {
    let path = i18n::split(path).map_or(path, |(_, rest)| rest);
    path.starts_with("/api/storefront/")
}

/// Cross-origin access to `/api/storefront/...`, so a front end on its
/// own domain can call it.
pub(crate) async fn cors(req: Request, next: Next) -> Response {
    if is_api(req.uri().path()) {
        CORS.handle(req, next).await
    } else {
        next.run(req).await
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        shop_profile,
        products_json,
        product_json,
        collections_json,
        collection_json,
        content_page,
        menu_json,
    ))
}

/// What a front end needs to know about the shop before its first page.
#[model]
pub struct ShopProfile {
    pub name: String,
    /// ISO 3166-1 alpha-2, e.g. `DE`; null until set.
    pub country: Option<String>,
    /// ISO 4217, e.g. `EUR`; null until set.
    pub currency: Option<String>,
    /// The language of the shop's own texts, e.g. `en`.
    pub default_language: String,
    /// Further languages it publishes, e.g. `["de"]`.
    pub languages: Vec<String>,
    /// The language this answer is in.
    pub language: String,
    /// The currency prices are shown in for `?country=`: the market's,
    /// else the shop's.
    pub prices_in: Option<String>,
}

/// The shop's transaction and the language and market the request asked
/// for.
async fn open(
    db: &Db,
    shop: &Shop,
    headers: &HeaderMap,
    params: &ListParams,
) -> Result<(TenantTx, Lang)> {
    let mut tx = shop.begin(db).await?;
    let asked = params
        .get("lang")
        .or_else(|| headers.get(LANGUAGE_HEADER).and_then(|v| v.to_str().ok()));
    let mut lang = Lang::asked(asked, shop_languages(&mut tx, shop).await?);
    if let Some(country) = params.get("country") {
        let country = country.to_ascii_uppercase();
        if country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()) {
            lang.market = soat_pricing::market_for(&mut tx, shop, &country).await?;
        }
    }
    Ok((tx, lang))
}

/// The shop's name, languages and currency (no sign-in). `?lang=` and
/// `?country=` say which language and prices the other endpoints answer
/// in.
#[get("/api/storefront/shop", tag = "storefront")]
async fn shop_profile(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Json<ShopProfile>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let (name, country, currency): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT name, country, currency FROM tenants WHERE id = $1")
            .bind(shop.tenant_id())
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    let prices_in = lang
        .market
        .as_ref()
        .map(|m| m.currency.clone())
        .or_else(|| currency.clone());
    Ok(Json(ShopProfile {
        name,
        country,
        currency,
        language: lang.code,
        default_language: lang.shop.default_language,
        languages: lang.shop.languages,
        prices_in,
    }))
}

/// The shop's active products (no sign-in), titles in `?lang=`, prices in
/// the market of `?country=`. `?q=` searches titles, `?tag=` filters,
/// `?collection=<handle>` keeps one collection, `?sort=` by `title` or
/// `price_minor`; 24 a page, at most 100.
#[get("/api/storefront/products", tag = "storefront")]
async fn products_json(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Json<Paged<ProductCard>>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let mut page = catalog::product_cards(&mut tx, &shop, &storage, &params).await?;
    i18n::cards(&mut tx, &shop, &lang, &mut page.items).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// An active product by handle, with pictures, options and variants (no
/// sign-in), in `?lang=` and the market of `?country=`. Drafts and
/// archived products are a 404.
#[get("/api/storefront/products/{handle}", tag = "storefront")]
async fn product_json(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    params: ListParams,
    Path(handle): Path<String>,
) -> Result<Json<ShopProduct>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let Some(mut p) = catalog::product_by_handle(&mut tx, &shop, &storage, &handle).await? else {
        return Err(Error::not_found());
    };
    i18n::product(&mut tx, &shop, &lang, &mut p).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// The shop's collections by title (no sign-in), in `?lang=`. `?q=`
/// searches titles; 50 a page, at most 100.
#[get("/api/storefront/collections", tag = "storefront")]
async fn collections_json(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Json<Paged<ShopCollection>>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let mut page = catalog::collections_page(&mut tx, &params).await?;
    for c in &mut page.items {
        i18n::collection(&mut tx, &shop, &lang, c).await?;
    }
    tx.commit().await?;
    Ok(Json(page))
}

/// A collection by handle (no sign-in), in `?lang=`. List its products
/// with `GET /api/storefront/products?collection=<handle>`.
#[get("/api/storefront/collections/{handle}", tag = "storefront")]
async fn collection_json(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
    Path(handle): Path<String>,
) -> Result<Json<ShopCollection>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let Some(mut c) = catalog::collection_by_handle(&mut tx, &shop, &handle).await? else {
        return Err(Error::not_found());
    };
    i18n::collection(&mut tx, &shop, &lang, &mut c).await?;
    tx.commit().await?;
    Ok(Json(c))
}

/// A published page by handle (no sign-in), its title and blocks in
/// `?lang=`. Drafts are a 404.
#[get("/api/storefront/pages/{handle}", tag = "storefront")]
async fn content_page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
    Path(handle): Path<String>,
) -> Result<Json<Page>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let Some(mut p) = soat_cms::published_page(&mut tx, &shop, &handle).await? else {
        return Err(Error::not_found());
    };
    i18n::page(&mut tx, &shop, &lang, &mut p).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// A menu by handle (no sign-in), its items in `?lang=` with links to
/// that language's addresses (`/de/...`).
#[get("/api/storefront/menus/{handle}", tag = "storefront")]
async fn menu_json(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
    Path(handle): Path<String>,
) -> Result<Json<Menu>> {
    let (mut tx, lang) = open(&db, &shop, &headers, &params).await?;
    let Some(mut m) = soat_cms::menu_by_handle(&mut tx, &shop, &handle).await? else {
        return Err(Error::not_found());
    };
    if let serde_json::Value::Array(items) = i18n::menu(&mut tx, &shop, &lang, &m).await? {
        m.items = items;
    }
    tx.commit().await?;
    Ok(Json(m))
}
