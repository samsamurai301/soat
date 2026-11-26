//! The storefront: the pages buyers browse, rendered on the server from the
//! shop's theme.
//!
//! `/` (the `home` content page if published, and new products),
//! `/products/{handle}`, `/collections/{handle}`, `/pages/{handle}`,
//! `/search?q=`, `/cart`, `/checkout`, `/order` (the buyer's last order),
//! `/account` (sign in, register, profile and orders), `/theme.css`,
//! `/robots.txt` and `/sitemap.xml`. Paths that match nothing get the
//! shop's redirect or its themed not-found page. The shop comes from the request's
//! `Host` (one of the shop's domains) or the `x-soat-tenant` header. The
//! cart lives in a cookie; adding to it and changing it are plain form
//! posts that htmx upgrades in place, so every page works without
//! JavaScript.

use serde_json::{json, Map, Value};
use soat_cart::storefront::{self as carts, CartToken};
use soat_cart::CartView;
use soat_catalog::search::{self, SearchQuery};
use soat_catalog::storefront::{self as catalog, ProductCard};
use soat_checkout::{CheckoutChange, CheckoutServices, CheckoutView, Complete};
use soat_core::address::country_name;
use soat_core::Currency;
use soat_customers::accounts::{self, SignedIn};
use soat_module::Module;
use soat_orders::{Order, OrderToken, PostalAddress};
use soat_payments::{Payment, Providers};
use soat_shipping::ShippingOption;
use soat_tenancy::{Scope, Shop, TenantTx};
use soat_theme::Theme;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;
use zapline::storage::Storage;

mod account;
mod blog;
mod headless;
mod i18n;
#[cfg(feature = "reviews")]
mod reviews;
mod seo;

use i18n::{root_of, Lang};

/// The cookie holding the buyer's cart: `<cart id>.<token>`.
pub const CART_COOKIE: &str = "soat_cart";

/// The cookie holding the buyer's last order: `<order id>.<token>`.
pub const ORDER_COOKIE: &str = "soat_order";

/// The storefront module.
pub struct Storefront;

impl Module for Storefront {
    fn name(&self) -> &'static str {
        "storefront"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "files",
            "customers",
            "catalog",
            "cart",
            "shipping",
            "payments",
            "orders",
            "checkout",
            "cms",
            "i18n",
            "pricing",
            "theme",
            #[cfg(feature = "apps")]
            "apps",
        ]
    }

    fn mount(&self, app: App) -> App {
        #[cfg(feature = "reviews")]
        let app = app.mount((reviews::review_form,));
        app.mount((
            home,
            product_page,
            collection_page,
            content_page,
            search_page,
            cart_page,
            cart_add,
            cart_recover,
            cart_line,
            checkout_page,
            checkout_update,
            checkout_complete,
            order_page,
            theme_css,
        ))
        .mount((
            account::login_page,
            account::login,
            account::register_page,
            account::register,
            account::logout,
            account::account_page,
            account::update_account,
            account::account_order,
            seo::robots,
            seo::sitemap,
            choose_country,
        ))
        .mount((
            account::forgot_page,
            account::forgot,
            account::access_page,
            account::set_password,
        ))
        .mount((blog::blog_page, blog::feed, blog::post_page))
        .plugin(headless::mount)
        .fallback(seo::fallback)
        .wrap(i18n::language_prefix)
        .wrap(headless::cors)
    }
}

fn money(minor: i64, currency: &str) -> String {
    Currency::parse(currency)
        .map(|c| c.format(minor))
        .unwrap_or_else(|_| format!("{minor} {currency}"))
}

fn card(c: &ProductCard) -> Value {
    let mut v = serde_json::to_value(c).unwrap_or_default();
    v["price"] = json!(money(c.price_minor, &c.currency));
    v
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The value of the request's cookie `name`, if not empty.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// The `<id>.<token>` in the request's cookie `name`.
fn id_cookie(headers: &HeaderMap, name: &str) -> Option<(String, String)> {
    cookie(headers, name)
        .as_deref()
        .and_then(|v| v.split_once('.'))
        .filter(|(id, token)| !id.is_empty() && !token.is_empty())
        .map(|(id, token)| (id.to_owned(), token.to_owned()))
}

/// The `<cart id>.<token>` in the request's cookies.
fn cart_cookie(headers: &HeaderMap) -> Option<(String, String)> {
    id_cookie(headers, CART_COOKIE)
}

/// `s` with its first letter capitalized, for messages shown as sentences.
fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some()
}

/// The shop's address, e.g. `https://shop.example`: the request's host when
/// it is one of the shop's domains, else the shop's first domain, else the
/// request's host; empty when there is none. `http` only for local hosts.
async fn origin(conn: &mut PgConnection, shop: &Shop, headers: &HeaderMap) -> Result<String> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_ascii_lowercase)
        .filter(|h| {
            !h.is_empty()
                && h.len() <= 255
                && h.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')
                })
        });
    let domains: Vec<(String,)> = sqlx::query_as(
        "SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY created_at, domain",
    )
    .bind(shop.tenant_id())
    .fetch_all(conn)
    .await?;
    let bare = |h: &str| h.split(':').next().unwrap_or_default().to_owned();
    let chosen = match host {
        Some(h) if domains.iter().any(|(d,)| *d == bare(&h)) => Some(h),
        _ => domains.into_iter().next().map(|(d,)| d).or(host),
    };
    Ok(chosen.map_or_else(String::new, |h| {
        let local = h.starts_with("localhost") || h.starts_with("127.") || h.starts_with('[');
        format!("{}://{h}", if local { "http" } else { "https" })
    }))
}

/// `url` made absolute on `origin` when it is a path.
fn absolute(origin: &str, url: &str) -> String {
    if url.starts_with('/') && !url.starts_with("//") {
        format!("{origin}{url}")
    } else {
        url.to_owned()
    }
}

/// Pages search engines should not list.
const NOINDEX: &[&str] = &[
    "cart",
    "checkout",
    "order",
    "login",
    "register",
    "account",
    "forgot",
    "access",
    "search",
    "not_found",
];

/// The blocks of installed apps the shop turned on: each script once
/// (`app_scripts`), and the blocks by placement (`app_blocks.body`,
/// `app_blocks.product`) for the theme to leave room for.
#[cfg(feature = "apps")]
async fn app_blocks(conn: &mut PgConnection, data: &mut Map<String, Value>) -> Result<()> {
    let blocks = soat_apps::storefront_blocks(conn).await?;
    let mut scripts: Vec<&str> = Vec::new();
    for b in &blocks {
        if !scripts.contains(&b.script_url.as_str()) {
            scripts.push(&b.script_url);
        }
    }
    let at = |placement: &str| -> Vec<Value> {
        blocks
            .iter()
            .filter(|b| b.placement == placement)
            .map(|b| json!({ "app": b.app_id, "handle": b.handle }))
            .collect()
    };
    data.insert(
        "app_blocks".to_owned(),
        json!({ "body": at("body"), "product": at("product") }),
    );
    data.insert("app_scripts".to_owned(), json!(scripts));
    Ok(())
}

/// One page being rendered: the shop's transaction, theme and the values
/// every page has.
struct View {
    tx: TenantTx,
    theme: Theme,
    data: Map<String, Value>,
    /// The signed-in customer's id.
    customer: Option<String>,
    lang: Lang,
}

impl View {
    async fn open(db: &Db, shop: &Shop, headers: &HeaderMap) -> Result<Self> {
        let mut tx = shop.begin(db).await?;
        let theme = soat_theme::load(&mut tx, shop).await?;
        let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
            .bind(shop.tenant_id())
            .fetch_one(&mut *tx)
            .await?;
        let mut lang = Lang::of(headers, soat_i18n::shop_languages(&mut tx, shop).await?);
        lang.market = i18n::market_of(&mut tx, shop, headers).await?;
        let country = cookie(headers, i18n::COUNTRY_COOKIE);
        let countries = i18n::countries(&mut tx, shop, country.as_deref()).await?;
        let mut menus = Map::new();
        for handle in ["main", "footer"] {
            if let Some(m) = soat_cms::menu_by_handle(&mut tx, shop, handle).await? {
                let items = i18n::menu(&mut tx, shop, &lang, &m).await?;
                menus.insert(handle.to_owned(), json!({ "items": items }));
            }
        }
        let count = match cart_cookie(headers) {
            Some((id, token)) => carts::held(&mut tx, shop, &id, &token)
                .await?
                .map_or(0, |c| c.item_count),
            None => 0,
        };
        let url = origin(&mut tx, shop, headers).await?;
        let path = headers
            .get(i18n::PATH_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("/")
            .to_owned();
        let mut data = Map::new();
        data.insert(
            "shop".to_owned(),
            json!({ "name": name, "url": url, "language": lang.code }),
        );
        data.insert("root".to_owned(), json!(lang.root));
        data.insert(
            "t".to_owned(),
            Value::Object(i18n::texts(&mut tx, shop, &lang).await?),
        );
        data.insert("languages".to_owned(), json!(lang.switcher(&path)));
        data.insert("countries".to_owned(), json!(countries));
        data.insert("here".to_owned(), json!(path));
        data.insert("menus".to_owned(), Value::Object(menus));
        data.insert("cart".to_owned(), json!({ "count": count }));
        #[cfg(feature = "apps")]
        app_blocks(&mut tx, &mut data).await?;
        let mut customer = None;
        if let Some(token) = cookie(headers, account::CUSTOMER_COOKIE) {
            let session = SignedIn::new(shop.clone(), &token);
            if let Some(id) = session.customer(&mut tx).await? {
                let me = accounts::profile(&mut tx, shop, &id).await?;
                data.insert(
                    "customer".to_owned(),
                    serde_json::to_value(me).unwrap_or_default(),
                );
                customer = Some(id);
            }
        }
        Ok(Self {
            tx,
            theme,
            data,
            customer,
            lang,
        })
    }

    fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_owned(), value);
    }

    /// The theme's text `key` in the page's language.
    fn t(&self, key: &str) -> String {
        self.data["t"][key].as_str().unwrap_or(key).to_owned()
    }

    async fn render(mut self, page: &str, status: StatusCode) -> Result<Response> {
        if NOINDEX.contains(&page) {
            self.set("noindex", json!(true));
        }
        if let Some(path) = self.data.get("canonical").and_then(Value::as_str) {
            let path = path.to_owned();
            let origin = self.data["shop"]["url"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            self.set("alternates", json!(self.lang.alternates(&origin, &path)));
            self.set("canonical", json!(self.lang.url(&path)));
        }
        self.tx.commit().await?;
        let html = self.theme.render(page, &Value::Object(self.data));
        let mut res = (status, Html(html)).into_response();
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        Ok(res)
    }

    /// Where the shop now sends `path` (a `301`), or the not-found page.
    async fn missing(mut self, shop: &Shop, path: &str) -> Result<Response> {
        match soat_cms::redirect_for(&mut self.tx, shop, path).await? {
            Some(to) => match HeaderValue::from_str(&self.lang.url(&to)) {
                Ok(location) => {
                    self.tx.commit().await?;
                    let mut res = StatusCode::MOVED_PERMANENTLY.into_response();
                    res.headers_mut().insert(header::LOCATION, location);
                    Ok(res)
                }
                Err(_) => self.not_found().await,
            },
            None => self.not_found().await,
        }
    }

    async fn not_found(mut self) -> Result<Response> {
        let title = self.t("not_found");
        self.set("page_title", json!(title));
        self.render("not_found", StatusCode::NOT_FOUND).await
    }
}

/// Blocks as themes read them: `is_<type>` set, images with `src`,
/// product blocks with their cards, columns as `{ blocks }`.
async fn blocks(
    conn: &mut PgConnection,
    shop: &Shop,
    storage: &Storage,
    lang: &Lang,
    blocks: &[Value],
) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        let mut b = b.clone();
        let kind = b["type"].as_str().unwrap_or_default().to_owned();
        b[format!("is_{kind}")] = json!(true);
        match kind.as_str() {
            "image" => {
                if let Some(file) = b["file_id"].as_str() {
                    let views = soat_files::views(conn, shop, storage, &[file.to_owned()]).await?;
                    b["src"] = json!(views.first().map(|v| v.url.clone()).unwrap_or_default());
                } else {
                    b["src"] = b["url"].clone();
                }
            }
            "products" => {
                let limit = b["limit"].as_i64().unwrap_or(8).clamp(1, 50);
                let mut cards = if let Some(id) = b["collection_id"].as_str() {
                    match catalog::collection_handle(conn, shop, id).await? {
                        Some(h) => {
                            let params = ListParams::from_query(&format!(
                                "collection={}&limit={limit}",
                                encode(&h)
                            ));
                            catalog::product_cards(conn, shop, storage, &params)
                                .await?
                                .items
                        }
                        None => Vec::new(),
                    }
                } else {
                    let ids: Vec<String> = b["product_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .take(limit as usize)
                        .collect();
                    catalog::cards_by_ids(conn, shop, storage, &ids).await?
                };
                i18n::cards(conn, shop, lang, &mut cards).await?;
                b["products"] = Value::Array(cards.iter().map(card).collect());
            }
            "columns" => {
                let mut cols = Vec::new();
                for col in b["columns"].as_array().cloned().unwrap_or_default() {
                    let inner = col.as_array().cloned().unwrap_or_default();
                    let inner = Box::pin(self::blocks(conn, shop, storage, lang, &inner)).await?;
                    cols.push(json!({ "blocks": inner }));
                }
                b["columns"] = Value::Array(cols);
            }
            _ => {}
        }
        out.push(b);
    }
    Ok(out)
}

/// The shop's home page: the `home` content page when published, and its
/// newest products.
#[get("/", tag = "storefront")]
async fn home(db: Db, shop: Shop, storage: Storage, headers: HeaderMap) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let name = v.data["shop"]["name"].clone();
    v.set("page_title", name);
    v.set("canonical", json!("/"));
    if let Some(mut page) = soat_cms::published_page(&mut v.tx, &shop, "home").await? {
        i18n::page(&mut v.tx, &shop, &v.lang, &mut page).await?;
        let b = blocks(&mut v.tx, &shop, &storage, &v.lang, &page.blocks).await?;
        let mut b = Value::Array(b);
        i18n::prefix_links(&v.lang.root, &mut b);
        v.set("blocks", b);
        if let Some(d) = page.seo_description {
            v.set("meta_description", json!(d));
        }
    }
    let params = ListParams::from_query("limit=8");
    let mut featured = catalog::product_cards(&mut v.tx, &shop, &storage, &params).await?;
    i18n::cards(&mut v.tx, &shop, &v.lang, &mut featured.items).await?;
    v.set(
        "featured",
        Value::Array(featured.items.iter().map(card).collect()),
    );
    v.render("index", StatusCode::OK).await
}

/// The query of a product page.
#[model]
pub struct ProductQuery {
    /// Set after the buyer sent a review.
    pub reviewed: Option<String>,
}

/// A product page.
#[get("/products/{handle}", tag = "storefront")]
async fn product_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    Path(handle): Path<String>,
    Query(q): Query<ProductQuery>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(mut p) = catalog::product_by_handle(&mut v.tx, &shop, &storage, &handle).await? else {
        return v.missing(&shop, &format!("/products/{handle}")).await;
    };
    i18n::product(&mut v.tx, &shop, &v.lang, &mut p).await?;
    let mut product = serde_json::to_value(&p).unwrap_or_default();
    product["price"] = json!(money(p.price_minor, &p.currency));
    if let Some(variants) = product["variants"].as_array_mut() {
        for (value, variant) in variants.iter_mut().zip(&p.variants) {
            value["price"] = json!(money(variant.price_minor, &variant.currency));
        }
    }
    let origin = v.data["shop"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let path = format!("/products/{}", p.handle);
    let summary = soat_core::markdown::to_text(&p.description, 160);
    v.set("page_title", json!(or(&p.seo_title, &p.title)));
    let description = or(&p.seo_description, &summary);
    if !description.is_empty() {
        v.set("meta_description", json!(description));
    }
    v.set("canonical", json!(path));
    v.set("og_type", json!("product"));
    let images: Vec<String> = p.images.iter().map(|i| absolute(&origin, &i.url)).collect();
    if let Some(first) = images.first() {
        v.set("og_image", json!(first));
    }
    let url = v.lang.url(&path);
    #[allow(unused_mut)]
    let mut ld = product_ld(&p, &origin, &url, images);
    #[cfg(feature = "reviews")]
    {
        let r = reviews::for_page(&mut v, &shop, &p.id).await?;
        if r["count"].as_i64().unwrap_or(0) > 0 {
            ld["aggregateRating"] = json!({
                "@type": "AggregateRating",
                "ratingValue": r["average"],
                "reviewCount": r["count"],
            });
        }
        v.set("reviews", r);
        v.set("reviews_on", json!(true));
    }
    v.set("reviewed", json!(q.reviewed.is_some()));
    v.set("product_ld", ld);
    v.set("product", product);
    v.render("product", StatusCode::OK).await
}

/// `own` unless it is empty, else `fallback`.
fn or<'a>(own: &'a str, fallback: &'a str) -> &'a str {
    if own.trim().is_empty() {
        fallback
    } else {
        own
    }
}

/// A product as schema.org structured data, for search results.
fn product_ld(p: &catalog::ShopProduct, origin: &str, path: &str, images: Vec<String>) -> Value {
    let decimal = |minor: i64, currency: &str| {
        Currency::parse(currency).map_or_else(|_| minor.to_string(), |c| c.decimal(minor))
    };
    let url = absolute(origin, path);
    let offers = if p.variants.is_empty() {
        json!({
            "@type": "Offer",
            "url": url,
            "price": decimal(p.price_minor, &p.currency),
            "priceCurrency": p.currency,
        })
    } else {
        let prices = p.variants.iter().map(|v| v.price_minor);
        let (low, high) = (prices.clone().min().unwrap_or(0), prices.max().unwrap_or(0));
        json!({
            "@type": "AggregateOffer",
            "url": url,
            "lowPrice": decimal(low, &p.currency),
            "highPrice": decimal(high, &p.currency),
            "offerCount": p.variants.len(),
            "priceCurrency": p.currency,
        })
    };
    json!({
        "@context": "https://schema.org",
        "@type": "Product",
        "name": p.title,
        "description": soat_core::markdown::to_text(&p.description, 5000),
        "url": url,
        "image": images,
        "offers": offers,
    })
}

/// Sets `products`, `prev_url` and `next_url` from a page of cards.
async fn list(v: &mut View, shop: &Shop, mut page: Paged<ProductCard>, base: &str) -> Result<()> {
    i18n::cards(&mut v.tx, shop, &v.lang, &mut page.items).await?;
    let base = v.lang.url(base);
    v.set(
        "products",
        Value::Array(page.items.iter().map(card).collect()),
    );
    let sep = if base.contains('?') { '&' } else { '?' };
    if page.offset > 0 {
        let prev = (page.offset - page.limit).max(0);
        v.set("prev_url", json!(format!("{base}{sep}offset={prev}")));
    }
    if page.offset + page.limit < page.total {
        v.set(
            "next_url",
            json!(format!("{base}{sep}offset={}", page.offset + page.limit)),
        );
    }
    Ok(())
}

fn offset(params: &ListParams) -> i64 {
    params
        .get("offset")
        .and_then(|o| o.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 100_000)
}

/// A collection page, 24 products a page (`?offset=`).
#[get("/collections/{handle}", tag = "storefront")]
async fn collection_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    params: ListParams,
    Path(handle): Path<String>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(mut c) = catalog::collection_by_handle(&mut v.tx, &shop, &handle).await? else {
        return v.missing(&shop, &format!("/collections/{handle}")).await;
    };
    i18n::collection(&mut v.tx, &shop, &v.lang, &mut c).await?;
    let query = format!(
        "collection={}&offset={}&sort={}",
        encode(&c.handle),
        offset(&params),
        params
            .get("sort")
            .map(encode)
            .unwrap_or_else(|| "-id".into())
    );
    let page =
        catalog::product_cards(&mut v.tx, &shop, &storage, &ListParams::from_query(&query)).await?;
    v.set("page_title", json!(or(&c.seo_title, &c.title)));
    let description = or(
        &c.seo_description,
        &soat_core::markdown::to_text(&c.description, 160),
    )
    .to_owned();
    if !description.is_empty() {
        v.set("meta_description", json!(description));
    }
    v.set("canonical", json!(format!("/collections/{}", c.handle)));
    list(&mut v, &shop, page, &format!("/collections/{}", c.handle)).await?;
    v.set("collection", serde_json::to_value(&c).unwrap_or_default());
    v.render("collection", StatusCode::OK).await
}

/// A content page.
#[get("/pages/{handle}", tag = "storefront")]
async fn content_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(mut p) = soat_cms::published_page(&mut v.tx, &shop, &handle).await? else {
        return v.missing(&shop, &format!("/pages/{handle}")).await;
    };
    i18n::page(&mut v.tx, &shop, &v.lang, &mut p).await?;
    let b = blocks(&mut v.tx, &shop, &storage, &v.lang, &p.blocks).await?;
    let mut b = Value::Array(b);
    i18n::prefix_links(&v.lang.root, &mut b);
    v.set(
        "page_title",
        json!(p.seo_title.clone().unwrap_or_else(|| p.title.clone())),
    );
    if let Some(d) = &p.seo_description {
        v.set("meta_description", json!(d));
    }
    v.set("canonical", json!(format!("/pages/{}", p.handle)));
    v.set("blocks", b);
    v.set("page", serde_json::to_value(&p).unwrap_or_default());
    v.render("page", StatusCode::OK).await
}

/// Search with filters: `q`, `tag`, `option.<Name>`, `price_from` and
/// `price_to` (in the shop's currency, e.g. `49.90`), `collection`, `sort`
/// and `offset`. Every product without any of them.
#[get("/search", tag = "storefront")]
async fn search_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let raw = uri.query().unwrap_or_default();
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw).unwrap_or_default();
    let mut query = SearchQuery::parse(raw);
    query.limit = 24;
    // Prices on the page are in the shop's main currency, not minor units.
    let currency = catalog::main_currency(&mut v.tx, &shop)
        .await?
        .and_then(|c| Currency::parse(&c).ok());
    let typed = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let (from, to) = (typed("price_from"), typed("price_to"));
    query.min_price = None;
    query.max_price = None;
    if let Some(c) = currency {
        query.min_price = from.as_deref().and_then(|p| c.parse_decimal(p));
        query.max_price = to.as_deref().and_then(|p| c.parse_decimal(p));
    }
    let found = search::search(&mut v.tx, &shop, &storage, &query).await?;
    let root = v.lang.root.clone();

    // Links keep every filter but the page.
    let keep: Vec<(String, String)> = pairs
        .iter()
        .filter(|(k, v)| k != "offset" && !v.trim().is_empty())
        .take(60)
        .cloned()
        .collect();
    let base = format!(
        "/search?{}",
        serde_urlencoded::to_string(&keep).unwrap_or_default()
    );
    let page = Paged {
        items: found.items,
        total: found.total,
        limit: found.limit,
        offset: found.offset,
    };
    list(&mut v, &shop, page, base.trim_end_matches('?')).await?;

    let sorts: Vec<Value> = [
        ("relevance", "best_match"),
        ("newest", "newest"),
        ("price", "price_low"),
        ("-price", "price_high"),
        ("title", "name"),
    ]
    .iter()
    .filter(|(s, _)| *s != "relevance" || !query.q.trim().is_empty())
    .map(|(s, label)| json!({ "value": s, "label": v.t(label), "selected": *s == found.sort }))
    .collect();
    let options: Vec<Value> = found
        .facets
        .options
        .iter()
        .map(|o| json!({ "name": o.name, "param": format!("option.{}", o.name), "values": o.values }))
        .collect();
    let price = found.facets.price.as_ref().map(|p| {
        let c = Currency::parse(&p.currency).ok();
        let show = |m: i64| c.map_or_else(|| m.to_string(), |c| c.decimal(m));
        json!({
            "currency": p.currency,
            "min": show(p.min_minor),
            "max": show(p.max_minor),
            "from": from,
            "to": to,
        })
    });
    let filtered =
        !query.tags.is_empty() || !query.options.is_empty() || from.is_some() || to.is_some();
    let q = query.q.clone();
    v.set(
        "page_title",
        json!(if q.is_empty() {
            v.t("all_products")
        } else {
            format!("{}: {q}", v.t("search"))
        }),
    );
    v.set("query", json!(q));
    v.set("total", json!(found.total));
    v.set("sorts", Value::Array(sorts));
    v.set(
        "facets",
        json!({ "tags": found.facets.tags, "options": options, "price": price }),
    );
    v.set("filtered", json!(filtered));
    v.set(
        "clear_url",
        json!(if q.is_empty() {
            format!("{root}/search")
        } else {
            format!("{root}/search?q={}", encode(&q))
        }),
    );
    v.set("collection_handle", json!(query.collection));
    v.render("search", StatusCode::OK).await
}

fn cart_value(cart: &CartView) -> Value {
    let lines: Vec<Value> = cart
        .lines
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "title": l.title,
                "quantity": l.quantity,
                "total": l.line_total_minor.map(|m| money(m, &cart.currency)),
                "issue": l.issue,
            })
        })
        .collect();
    json!({
        "id": cart.id,
        "count": cart.item_count,
        "lines": lines,
        "subtotal": money(cart.subtotal_minor, &cart.currency),
        "ready": cart.ready,
    })
}

/// The buyer's cart.
#[get("/cart", tag = "storefront")]
async fn cart_page(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    render_cart(&db, &shop, &headers).await
}

async fn render_cart(db: &Db, shop: &Shop, headers: &HeaderMap) -> Result<Response> {
    let mut v = View::open(db, shop, headers).await?;
    let cart = match cart_cookie(headers) {
        Some((id, token)) => carts::held(&mut v.tx, shop, &id, &token).await?,
        None => None,
    };
    let title = v.t("your_cart");
    v.set("page_title", json!(title));
    if let Some(c) = &cart {
        v.set("cart", cart_value(c));
    }
    v.render("cart", StatusCode::OK).await
}

/// A form post adding to the cart.
#[model]
pub struct AddToCart {
    pub product_id: String,
    /// The chosen variant, when the product has variants.
    pub variant_id: Option<String>,
    pub quantity: Option<i64>,
}

fn cart_link(headers: &HeaderMap, count: Option<i64>, notice: Option<&str>) -> Response {
    let count = count.map(|n| format!(" ({n})")).unwrap_or_default();
    let root = root_of(headers);
    let lang = root.trim_start_matches('/');
    let label = soat_theme::strings(if lang.is_empty() { "en" } else { lang })["cart"]
        .as_str()
        .unwrap_or("Cart")
        .to_owned();
    let mut out =
        format!("<a class=\"cart-link\" href=\"{root}/cart\" id=\"cart-count\">{label}{count}</a>");
    if let Some(n) = notice {
        let mut escaped = String::new();
        for c in n.chars() {
            match c {
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '&' => escaped.push_str("&amp;"),
                '"' => escaped.push_str("&quot;"),
                c => escaped.push(c),
            }
        }
        out.push_str(&format!(
            " <span class=\"notice\" role=\"alert\">{escaped}</span>"
        ));
    }
    Html(out).into_response()
}

fn set_cookie(res: &mut Response, value: &str, secure: bool) {
    put_cookie(res, CART_COOKIE, value, 2_592_000, secure);
}

/// Sets cookie `name`; `max_age` 0 removes it.
fn put_cookie(res: &mut Response, name: &str, value: &str, max_age: i64, secure: bool) {
    let cookie = format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
}

/// Add to the cart (form post from a product page). Starts a cart, kept in
/// a cookie, when there is none. Answers htmx with the cart link, others
/// with a redirect to the cart.
#[post("/cart/add", tag = "storefront")]
async fn cart_add(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<AddToCart>,
) -> Result<Response> {
    let quantity = f.quantity.unwrap_or(1).clamp(1, 99);
    let item = f
        .variant_id
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or(f.product_id.clone());
    // Only what the storefront shows can be bought.
    let mut tx = shop.begin(&db).await?;
    let shown = catalog::cards_by_ids(
        &mut tx,
        &shop,
        &storage,
        std::slice::from_ref(&f.product_id),
    )
    .await?;
    let Some(product) = shown.first() else {
        return Err(Error::unprocessable("this product is not for sale").with_code("unknown_item"));
    };
    // The buyer's market's currency when the item is sold in it.
    let mut currency = product.currency.clone();
    if let Some(m) = i18n::market_of(&mut tx, &shop, &headers).await? {
        if let Ok(c) = Currency::parse(&m.currency) {
            if soat_pricing::resolve(&mut tx, &shop, &item, c)
                .await?
                .is_some()
            {
                currency = m.currency;
            }
        }
    }
    let held = match cart_cookie(&headers) {
        Some((id, token)) => carts::held(&mut tx, &shop, &id, &token)
            .await?
            .map(|c| (c.id, token)),
        None => None,
    };
    let (outcome, cookie) = match held {
        Some((id, token)) => {
            tx.commit().await?;
            let holder = CartToken::new(shop.clone(), &token);
            let (mut tx, id) = holder.open(&db, &id).await?;
            let out = carts::add(&mut tx, &shop, &id, &item, quantity).await;
            if out.is_ok() {
                tx.commit().await?;
            }
            (out, None)
        }
        None => {
            let (token, cart) = carts::create(&mut tx, &shop, &currency, None).await?;
            let out = carts::add(&mut tx, &shop, &cart.id, &item, quantity).await;
            if out.is_ok() {
                tx.commit().await?;
            }
            (out, Some(format!("{}.{token}", cart.id)))
        }
    };
    let mut res = match (&outcome, is_htmx(&headers)) {
        (Ok(cart), true) => cart_link(&headers, Some(cart.item_count), None),
        (Ok(_), false) => Redirect::to(&format!("{}/cart", root_of(&headers))).into_response(),
        (Err(e), true) => cart_link(&headers, None, Some(&upper_first(e.message()))),
        (Err(_), false) => return outcome.map(|_| StatusCode::OK.into_response()),
    };
    if outcome.is_ok() {
        if let Some(c) = cookie {
            set_cookie(&mut res, &c, conn.secure);
        }
    }
    Ok(res)
}

/// The query of a recovery link.
#[model]
pub struct RecoverQuery {
    pub token: String,
}

/// The link in an abandoned cart email: puts the cart back in this
/// browser and shows it. A link that no longer works shows the cart this
/// browser has.
#[get("/cart/recover", tag = "storefront")]
async fn cart_recover(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    Query(q): Query<RecoverQuery>,
) -> Result<Response> {
    let mut res = Redirect::to(&format!("{}/cart", root_of(&headers))).into_response();
    if let Ok((token, cart)) = soat_cart::recovery::recover(&db, &shop, &q.token).await {
        set_cookie(&mut res, &format!("{}.{token}", cart.id), conn.secure);
    }
    Ok(res)
}

/// A form post changing a line's quantity.
#[model]
pub struct LineQuantity {
    /// 0 removes the line.
    pub quantity: i64,
}

/// Change a cart line's quantity; 0 removes it (form post from the cart).
#[post("/cart/lines/{line_id}", tag = "storefront")]
async fn cart_line(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Form(f): Form<LineQuantity>,
) -> Result<Response> {
    let Some((id, token)) = cart_cookie(&headers) else {
        return Ok(Redirect::to(&format!("{}/cart", root_of(&headers))).into_response());
    };
    let holder = CartToken::new(shop.clone(), &token);
    let (mut tx, id) = holder.open(&db, &id).await?;
    carts::set_quantity(&mut tx, &shop, &id, &line_id, f.quantity.clamp(0, 999)).await?;
    tx.commit().await?;
    if is_htmx(&headers) {
        render_cart(&db, &shop, &headers).await
    } else {
        Ok(Redirect::to(&format!("{}/cart", root_of(&headers))).into_response())
    }
}

/// The checkout form: contact, shipping address, shipping choice and
/// discount code. Empty fields are left as they are.
#[model]
pub struct CheckoutForm {
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    pub line1: Option<String>,
    pub line2: Option<String>,
    pub postal_code: Option<String>,
    pub city: Option<String>,
    /// ISO 3166-1 alpha-2.
    pub country: Option<String>,
    pub phone: Option<String>,
    pub vat_number: Option<String>,
    pub shipping_rate_id: Option<String>,
    /// Empty removes the code.
    pub discount_code: Option<String>,
    /// A gift card's code; empty leaves the card as it is.
    pub gift_card_code: Option<String>,
    /// `on` removes the gift card.
    pub remove_gift_card: Option<String>,
    /// Set when the store credit box was shown; then `use_store_credit`
    /// is `on` or absent.
    pub store_credit_shown: Option<String>,
    pub use_store_credit: Option<String>,
}

fn field(v: &Option<String>) -> String {
    v.as_deref().unwrap_or_default().trim().to_owned()
}

impl CheckoutForm {
    /// The address typed, unless every line of it is empty.
    fn address(&self) -> Option<PostalAddress> {
        let a = PostalAddress {
            first_name: field(&self.first_name),
            last_name: field(&self.last_name),
            company: field(&self.company),
            line1: field(&self.line1),
            line2: field(&self.line2),
            postal_code: field(&self.postal_code),
            city: field(&self.city),
            region: String::new(),
            country: field(&self.country).to_ascii_uppercase(),
            phone: field(&self.phone),
        };
        (!a.line1.is_empty() || !a.city.is_empty()).then_some(a)
    }
}

fn days(v: &View, o: &ShippingOption) -> Option<String> {
    let (d, up_to, from) = (v.t("days"), v.t("up_to"), v.t("from").to_lowercase());
    match (o.min_days, o.max_days) {
        (Some(a), Some(b)) if a == b => Some(format!("{a} {d}")),
        (Some(a), Some(b)) => Some(format!("{a}–{b} {d}")),
        (None, Some(b)) => Some(format!("{up_to} {b} {d}")),
        (Some(a), None) => Some(format!("{from} {a} {d}")),
        (None, None) => None,
    }
}

/// Fills the checkout page's values. `typed` is a form that failed, shown
/// back as the buyer typed it.
async fn checkout_values(
    v: &mut View,
    shop: &Shop,
    c: &CheckoutView,
    typed: Option<&CheckoutForm>,
) -> Result<()> {
    let cur = c.cart.currency.as_str();
    let address = match typed {
        Some(f) => f
            .address()
            .map(|a| serde_json::to_value(a).unwrap_or_default()),
        None => c
            .shipping_address
            .as_ref()
            .map(|a| serde_json::to_value(a).unwrap_or_default()),
    }
    .unwrap_or_else(|| json!({}));
    let mut codes = soat_shipping::countries(&mut v.tx, shop).await?;
    if codes.is_empty() {
        codes = soat_core::tax::EU_COUNTRIES
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
    }
    let chosen = address["country"].as_str().unwrap_or_default().to_owned();
    if !chosen.is_empty() && !codes.contains(&chosen) {
        codes.push(chosen.clone());
    }
    let mut countries: Vec<(String, String)> = codes
        .into_iter()
        .map(|code| {
            let name = country_name(&code).map_or_else(|| code.clone(), str::to_owned);
            (name, code)
        })
        .collect();
    countries.sort();
    let countries: Vec<Value> = countries
        .into_iter()
        .map(|(name, code)| json!({ "selected": code == chosen, "code": code, "name": name }))
        .collect();
    let chosen_rate = c.shipping.as_ref().map(|s| s.rate_id.as_str());
    let options: Vec<Value> = c
        .shipping_options
        .iter()
        .map(|o| {
            json!({
                "rate_id": o.rate_id,
                "name": o.name,
                "price": money(o.price_minor, &o.currency),
                "days": days(v, o),
                "selected": Some(o.rate_id.as_str()) == chosen_rate,
            })
        })
        .collect();
    let methods = soat_payments::public_methods(&mut v.tx, shop).await?;
    let methods: Vec<Value> = methods
        .iter()
        .enumerate()
        .map(|(i, m)| json!({ "id": m.id, "name": m.name, "checked": i == 0 }))
        .collect();
    let email = typed
        .and_then(|f| f.email.clone())
        .unwrap_or_else(|| c.email.clone());
    let mut checkout = serde_json::to_value(c).unwrap_or_default();
    checkout["email"] = json!(email);
    if let Some(f) = typed {
        checkout["vat_number"] = json!(field(&f.vat_number));
    }
    let discount_code = match typed {
        Some(f) => field(&f.discount_code),
        None => c
            .discount
            .as_ref()
            .and_then(|d| d.code.clone())
            .unwrap_or_default(),
    };
    let title = v.t("checkout");
    v.set("page_title", json!(title));
    v.set("checkout", checkout);
    v.set("address", address);
    v.set("countries", Value::Array(countries));
    v.set("shipping_options", Value::Array(options));
    v.set("discount_code", json!(discount_code));
    v.set("payment_methods", Value::Array(methods));
    v.set("lines", cart_value(&c.cart)["lines"].clone());
    v.set("ready", json!(c.ready));
    let some = |m: i64| (m != 0).then(|| money(m, cur));
    v.set(
        "totals",
        json!({
            "subtotal": money(c.subtotal_minor, cur),
            "shipping": c.shipping.as_ref().map(|_| money(c.shipping_minor, cur)),
            "discount": some(c.discount_minor),
            "tax": some(c.tax_minor),
            "total": money(c.total_minor, cur),
            "gift_card": c.gift_card.as_ref().and_then(|g| some(g.applied_minor)),
            "store_credit": c.store_credit.as_ref().and_then(|g| some(g.applied_minor)),
            "due": money(c.amount_due_minor, cur),
            "paid_by_balance": c.amount_due_minor < c.total_minor,
        }),
    );
    v.set("due", json!(c.amount_due_minor > 0));
    if let Some(sc) = &c.store_credit {
        v.set("store_credit_balance", json!(money(sc.balance_minor, cur)));
    }
    Ok(())
}

/// The checkout page, or a redirect to the cart when there is nothing to
/// check out. `error` is shown, and `typed` is a form shown back as typed.
async fn render_checkout(
    db: &Db,
    shop: &Shop,
    services: &CheckoutServices,
    headers: &HeaderMap,
    error: Option<String>,
    typed: Option<&CheckoutForm>,
) -> Result<Response> {
    let mut v = View::open(db, shop, headers).await?;
    let cart = match cart_cookie(headers) {
        Some((id, token)) => carts::held(&mut v.tx, shop, &id, &token).await?,
        None => None,
    };
    let Some(mut cart) = cart.filter(|c| !c.lines.is_empty()) else {
        v.tx.commit().await?;
        return Ok(Redirect::to(&format!("{}/cart", root_of(headers))).into_response());
    };
    if let (Some(customer), None) = (v.customer.clone(), &cart.customer_id) {
        cart = carts::attach(&mut v.tx, shop, &cart.id, &customer).await?;
    }
    let c = soat_checkout::checkout(&mut v.tx, shop, services, &cart.id).await?;
    checkout_values(&mut v, shop, &c, typed).await?;
    let status = if let Some(e) = error {
        v.set("error", json!(e));
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    v.render("checkout", status).await
}

/// The checkout: contact, address, shipping, discount code, then payment.
#[get("/checkout", tag = "storefront")]
async fn checkout_page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    services: CheckoutServices,
) -> Result<Response> {
    render_checkout(&db, &shop, &services, &headers, None, None).await
}

fn blank_change() -> CheckoutChange {
    CheckoutChange {
        email: None,
        shipping_address: None,
        billing_address: None,
        vat_number: None,
        shipping_rate_id: None,
        discount_code: None,
        gift_card_code: None,
        use_store_credit: None,
    }
}

/// Saves the checkout form (a form post). The shipping choice falls back
/// to the cheapest option that applies; a discount code that does not
/// apply is shown and the rest is kept.
#[post("/checkout", tag = "storefront")]
async fn checkout_update(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    services: CheckoutServices,
    Form(f): Form<CheckoutForm>,
) -> Result<Response> {
    let Some((id, token)) = cart_cookie(&headers) else {
        return Ok(Redirect::to(&format!("{}/cart", root_of(&headers))).into_response());
    };
    let holder = CartToken::new(shop.clone(), &token);
    let (mut tx, id) = holder.open(&db, &id).await?;
    let email = Some(field(&f.email)).filter(|e| !e.is_empty());
    if let Some(e) = &email {
        if !e.contains('@') || e.len() > 254 {
            drop(tx);
            let msg = format!("{e} is not an email address");
            return render_checkout(&db, &shop, &services, &headers, Some(msg), Some(&f)).await;
        }
    }
    let change = CheckoutChange {
        email,
        shipping_address: f.address(),
        vat_number: f.vat_number.as_ref().map(|v| v.trim().to_owned()),
        ..blank_change()
    };
    let view = match soat_checkout::update(&mut tx, &shop, &services, &id, &change).await {
        Ok(v) => v,
        Err(e) => {
            drop(tx);
            return render_checkout(
                &db,
                &shop,
                &services,
                &headers,
                Some(upper_first(e.message())),
                Some(&f),
            )
            .await;
        }
    };
    let asked = Some(field(&f.shipping_rate_id)).filter(|r| !r.is_empty());
    let rate = asked
        .filter(|r| view.shipping_options.iter().any(|o| &o.rate_id == r))
        .or_else(|| {
            view.shipping
                .is_none()
                .then(|| view.shipping_options.first().map(|o| o.rate_id.clone()))
                .flatten()
        });
    if let Some(rate) = rate {
        let change = CheckoutChange {
            shipping_rate_id: Some(rate),
            ..blank_change()
        };
        soat_checkout::update(&mut tx, &shop, &services, &id, &change).await?;
    }
    let mut problem = None;
    if let Some(code) = f.discount_code.as_ref().map(|c| c.trim().to_owned()) {
        let current = view.discount.as_ref().and_then(|d| d.code.clone());
        if current.as_deref().unwrap_or_default() != code {
            let change = CheckoutChange {
                discount_code: Some(code),
                ..blank_change()
            };
            if let Err(e) = soat_checkout::update(&mut tx, &shop, &services, &id, &change).await {
                problem = Some(upper_first(e.message()));
                let back = CheckoutChange {
                    discount_code: Some(current.unwrap_or_default()),
                    ..blank_change()
                };
                soat_checkout::update(&mut tx, &shop, &services, &id, &back).await?;
            }
        }
    }
    let gift = if f.remove_gift_card.as_deref() == Some("on") {
        Some(String::new())
    } else {
        Some(field(&f.gift_card_code)).filter(|g| !g.is_empty())
    };
    let store_credit = f
        .store_credit_shown
        .as_ref()
        .map(|_| f.use_store_credit.as_deref() == Some("on"));
    if gift.is_some() || store_credit.is_some() {
        let change = CheckoutChange {
            gift_card_code: gift,
            use_store_credit: store_credit,
            ..blank_change()
        };
        if let Err(e) = soat_checkout::update(&mut tx, &shop, &services, &id, &change).await {
            problem.get_or_insert(upper_first(e.message()));
            let change = CheckoutChange {
                use_store_credit: store_credit,
                ..blank_change()
            };
            soat_checkout::update(&mut tx, &shop, &services, &id, &change).await?;
        }
    }
    tx.commit().await?;
    match problem {
        Some(p) => render_checkout(&db, &shop, &services, &headers, Some(p), Some(&f)).await,
        None => Ok(Redirect::to(&format!("{}/checkout", root_of(&headers))).into_response()),
    }
}

/// The payment step of the checkout form.
#[model]
pub struct PlaceOrder {
    /// Not needed when a gift card or store credit pays it all.
    pub payment_method_id: Option<String>,
}

/// The address buyers come back to from a payment provider.
fn return_url(headers: &HeaderMap, secure: bool) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    ok.then(|| {
        format!(
            "{}://{host}{}/order",
            if secure { "https" } else { "http" },
            root_of(headers)
        )
    })
}

/// Places the order (a form post): sends the buyer to the payment provider
/// when it takes over, otherwise to the order's page. The order is kept in
/// a cookie and the cart's cookie is dropped.
#[post("/checkout/complete", tag = "storefront")]
async fn checkout_complete(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    providers: State<Providers>,
    services: CheckoutServices,
    Form(f): Form<PlaceOrder>,
) -> Result<Response> {
    let Some((id, token)) = cart_cookie(&headers) else {
        return Ok(Redirect::to(&format!("{}/cart", root_of(&headers))).into_response());
    };
    let holder = CartToken::new(shop.clone(), &token);
    let req = Complete {
        payment_method_id: f.payment_method_id,
        return_url: return_url(&headers, conn.secure),
    };
    let done = match soat_checkout::complete(&db, &providers, &services, &holder, &id, req).await {
        Ok(d) => d,
        Err(e) => {
            return render_checkout(
                &db,
                &shop,
                &services,
                &headers,
                Some(upper_first(e.message())),
                None,
            )
            .await;
        }
    };
    let next = done
        .payment
        .as_ref()
        .and_then(|p| p.next_action.as_ref())
        .filter(|a| a["type"] == "redirect")
        .and_then(|a| a["url"].as_str())
        .filter(|u| u.starts_with("https://"))
        .map_or_else(|| format!("{}/order", root_of(&headers)), str::to_owned);
    let mut res = Redirect::to(&next).into_response();
    put_cookie(
        &mut res,
        ORDER_COOKIE,
        &format!("{}.{}", done.order.id, done.order_token),
        2_592_000,
        conn.secure,
    );
    put_cookie(&mut res, CART_COOKIE, "", 0, conn.secure);
    Ok(res)
}

fn payment_status(status: &str) -> &'static str {
    match status {
        "paid" => "payment_paid",
        "authorized" => "payment_authorized",
        "failed" => "payment_failed",
        "canceled" => "payment_canceled",
        "refunded" | "partially_refunded" => "payment_refunded",
        _ => "payment_pending",
    }
}

/// The buyer's last order, from its cookie: what was bought and how to pay.
#[get("/order", tag = "storefront")]
async fn order_page(db: Db, shop: Shop, storage: Storage, headers: HeaderMap) -> Result<Response> {
    let found = match id_cookie(&headers, ORDER_COOKIE) {
        Some((id, token)) => match OrderToken::new(shop.clone(), &token).open(&db, &id).await {
            Ok((mut tx, id)) => {
                let order = soat_orders::load(&mut tx, &shop, &id).await?;
                let payments = soat_payments::for_reference(&mut tx, &shop, &id).await?;
                let downloads = match &order {
                    Some(o) => {
                        soat_orders::downloads::ready_downloads(&mut tx, &shop, &storage, o).await?
                    }
                    None => Vec::new(),
                };
                tx.commit().await?;
                order.map(|o| (o, payments, downloads))
            }
            Err(_) => None,
        },
        None => None,
    };
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some((order, payments, downloads)) = found else {
        return v.not_found().await;
    };
    order_values(&mut v, &order, &payments);
    download_values(&mut v, &downloads);
    v.render("order", StatusCode::OK).await
}

/// The order page's download links, once the order is paid.
pub(crate) fn download_values(v: &mut View, downloads: &[soat_orders::downloads::Download]) {
    let list: Vec<Value> = downloads
        .iter()
        .map(|d| json!({ "title": d.title, "file_name": d.file.file_name, "url": d.file.url }))
        .collect();
    v.set("downloads", json!(list));
}

/// Sets the order page's values.
fn order_values(v: &mut View, order: &Order, payments: &[Payment]) {
    let cur = order.currency.as_str();
    let lines: Vec<Value> = order
        .lines
        .iter()
        .map(|l| json!({ "title": l.title, "quantity": l.quantity, "total": money(l.total_minor, cur) }))
        .collect();
    let payment = payments.first();
    let instructions = payment
        .filter(|p| {
            !matches!(
                p.status.as_str(),
                "paid" | "refunded" | "partially_refunded"
            )
        })
        .and_then(|p| p.next_action.as_ref())
        .filter(|a| a["type"] == "instructions")
        .and_then(|a| a["text"].as_str())
        .map(str::to_owned);
    let some = |m: i64| (m != 0).then(|| money(m, cur));
    v.set("page_title", json!(format!("Order #{}", order.number)));
    v.set("lines", Value::Array(lines));
    v.set(
        "totals",
        json!({
            "subtotal": money(order.subtotal_minor, cur),
            "shipping": some(order.shipping_minor),
            "discount": some(order.discount_minor),
            "tax": some(order.tax_minor),
            "total": money(order.total_minor, cur),
        }),
    );
    v.set("instructions", json!(instructions));
    let paying = payment.map_or(
        if order.total_minor == 0 {
            "payment_none"
        } else {
            "payment_pending"
        },
        |p| payment_status(&p.status),
    );
    let paying = v.t(paying);
    v.set("payment_status", json!(paying));
    v.set(
        "placed",
        json!(order.placed_at.get(..10).unwrap_or_default()),
    );
    let status = v.t(order_status(
        &order.status,
        &order.financial_status,
        &order.fulfillment_status,
    ));
    v.set("status", json!(status));
    v.set("order", serde_json::to_value(order).unwrap_or_default());
}

/// The text key of an order's state in a buyer's words.
fn order_status(status: &str, financial: &str, fulfillment: &str) -> &'static str {
    match (status, financial, fulfillment) {
        ("canceled", ..) => "canceled",
        (_, "refunded", _) => "refunded",
        (_, _, "fulfilled") => "shipped",
        (_, _, "partially_fulfilled") => "partly_shipped",
        (_, "paid" | "partially_refunded", _) => "paid",
        _ => "awaiting_payment",
    }
}

/// The country picker's fields.
#[model]
pub struct CountryForm {
    /// e.g. `CH`; empty for none.
    pub country: String,
    /// The page to go back to, e.g. `/de/products/x`.
    pub back: Option<String>,
}

/// Pick the buyer's country (a form post): prices show in its market's
/// currency, and the cart moves to it. Remembered in a cookie.
#[post("/country", tag = "storefront")]
async fn choose_country(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<CountryForm>,
) -> Result<Response> {
    let country = f.country.trim().to_ascii_uppercase();
    let known = soat_core::address::country_name(&country).is_some();
    let mut tx = shop.begin(&db).await?;
    let market = if known {
        soat_pricing::market_for(&mut tx, &shop, &country).await?
    } else {
        None
    };
    let currency = match &market {
        Some(m) => Some(m.currency.clone()),
        None => catalog::main_currency(&mut tx, &shop).await?,
    };
    let held = match cart_cookie(&headers) {
        Some((id, token)) => carts::held(&mut tx, &shop, &id, &token)
            .await?
            .filter(|c| Some(&c.currency) != currency.as_ref())
            .map(|c| (c.id, token)),
        None => None,
    };
    tx.commit().await?;
    if let (Some((id, token)), Some(currency)) = (held, &currency) {
        let holder = CartToken::new(shop.clone(), &token);
        let (mut tx, id) = holder.open(&db, &id).await?;
        carts::set_currency(&mut tx, &shop, &id, currency).await?;
        tx.commit().await?;
    }
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
    if known {
        put_cookie(
            &mut res,
            i18n::COUNTRY_COOKIE,
            &country,
            365 * 86_400,
            conn.secure,
        );
    } else {
        put_cookie(&mut res, i18n::COUNTRY_COOKIE, "", 0, conn.secure);
    }
    Ok(res)
}

/// The theme's stylesheet.
#[get("/theme.css", tag = "storefront")]
async fn theme_css(db: Db, shop: Shop) -> Result<Response> {
    let mut tx = shop.begin(&db).await?;
    let theme = soat_theme::load(&mut tx, &shop).await?;
    tx.commit().await?;
    let mut res = theme.css().to_owned().into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cart_cookies() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; soat_cart=cart_1.tok; b=2"),
        );
        assert_eq!(cart_cookie(&h), Some(("cart_1".into(), "tok".into())));
        assert_eq!(cart_cookie(&HeaderMap::new()), None);
        assert_eq!(money(12999, "EUR"), "129.99 EUR");
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
        h.insert(header::COOKIE, HeaderValue::from_static("soat_order=.x"));
        assert_eq!(id_cookie(&h, ORDER_COOKIE), None);
        h.insert(header::HOST, HeaderValue::from_static("shop.example:8080"));
        assert_eq!(
            return_url(&h, true).as_deref(),
            Some("https://shop.example:8080/order")
        );
        h.insert(header::HOST, HeaderValue::from_static("evil/path"));
        assert_eq!(return_url(&h, false), None);
    }
}
