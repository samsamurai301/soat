//! The storefront: the pages buyers browse, rendered on the server from the
//! shop's theme.
//!
//! `/` (the `home` content page if published, and new products),
//! `/products/{handle}`, `/collections/{handle}`, `/pages/{handle}`,
//! `/search?q=`, `/cart` and `/theme.css`. The shop comes from the request's
//! `Host` (one of the shop's domains) or the `x-soat-tenant` header. The
//! cart lives in a cookie; adding to it and changing it are plain form
//! posts that htmx upgrades in place, so every page works without
//! JavaScript.

use serde_json::{json, Map, Value};
use soat_cart::storefront::{self as carts, CartToken};
use soat_cart::CartView;
use soat_catalog::storefront::{self as catalog, ProductCard};
use soat_core::Currency;
use soat_module::Module;
use soat_tenancy::{Scope, Shop, TenantTx};
use soat_theme::Theme;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;
use zapline::storage::Storage;

/// The cookie holding the buyer's cart: `<cart id>.<token>`.
pub const CART_COOKIE: &str = "soat_cart";

/// The storefront module.
pub struct Storefront;

impl Module for Storefront {
    fn name(&self) -> &'static str {
        "storefront"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["files", "catalog", "cart", "cms", "theme"]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            home,
            product_page,
            collection_page,
            content_page,
            search_page,
            cart_page,
            cart_add,
            cart_line,
            theme_css,
        ))
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

/// The `<cart id>.<token>` in the request's cookies.
fn cart_cookie(headers: &HeaderMap) -> Option<(String, String)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(CART_COOKIE)?.strip_prefix('='))
        .and_then(|v| v.split_once('.'))
        .map(|(id, token)| (id.to_owned(), token.to_owned()))
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some()
}

/// One page being rendered: the shop's transaction, theme and the values
/// every page has.
struct View {
    tx: TenantTx,
    theme: Theme,
    data: Map<String, Value>,
}

impl View {
    async fn open(db: &Db, shop: &Shop, headers: &HeaderMap) -> Result<Self> {
        let mut tx = shop.begin(db).await?;
        let theme = soat_theme::load(&mut tx, shop).await?;
        let (name,): (String,) = sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
            .bind(shop.tenant_id())
            .fetch_one(&mut *tx)
            .await?;
        let mut menus = Map::new();
        for handle in ["main", "footer"] {
            if let Some(m) = soat_cms::menu_by_handle(&mut tx, shop, handle).await? {
                menus.insert(handle.to_owned(), json!({ "items": m.items }));
            }
        }
        let count = match cart_cookie(headers) {
            Some((id, token)) => carts::held(&mut tx, shop, &id, &token)
                .await?
                .map_or(0, |c| c.item_count),
            None => 0,
        };
        let mut data = Map::new();
        data.insert("shop".to_owned(), json!({ "name": name }));
        data.insert("menus".to_owned(), Value::Object(menus));
        data.insert("cart".to_owned(), json!({ "count": count }));
        Ok(Self { tx, theme, data })
    }

    fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_owned(), value);
    }

    async fn render(self, page: &str, status: StatusCode) -> Result<Response> {
        self.tx.commit().await?;
        let html = self.theme.render(page, &Value::Object(self.data));
        let mut res = (status, Html(html)).into_response();
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        Ok(res)
    }

    async fn not_found(mut self) -> Result<Response> {
        self.set("page_title", json!("Not found"));
        self.render("not_found", StatusCode::NOT_FOUND).await
    }
}

/// Blocks as themes read them: `is_<type>` set, images with `src`,
/// product blocks with their cards, columns as `{ blocks }`.
async fn blocks(
    conn: &mut PgConnection,
    shop: &Shop,
    storage: &Storage,
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
                let cards = if let Some(id) = b["collection_id"].as_str() {
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
                b["products"] = Value::Array(cards.iter().map(card).collect());
            }
            "columns" => {
                let mut cols = Vec::new();
                for col in b["columns"].as_array().cloned().unwrap_or_default() {
                    let inner = col.as_array().cloned().unwrap_or_default();
                    let inner = Box::pin(self::blocks(conn, shop, storage, &inner)).await?;
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
    if let Some(page) = soat_cms::published_page(&mut v.tx, &shop, "home").await? {
        let b = blocks(&mut v.tx, &shop, &storage, &page.blocks).await?;
        v.set("blocks", Value::Array(b));
        if let Some(d) = page.seo_description {
            v.set("meta_description", json!(d));
        }
    }
    let params = ListParams::from_query("limit=8");
    let featured = catalog::product_cards(&mut v.tx, &shop, &storage, &params).await?;
    v.set(
        "featured",
        Value::Array(featured.items.iter().map(card).collect()),
    );
    v.render("index", StatusCode::OK).await
}

/// A product page.
#[get("/products/{handle}", tag = "storefront")]
async fn product_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(p) = catalog::product_by_handle(&mut v.tx, &shop, &storage, &handle).await? else {
        return v.not_found().await;
    };
    let mut product = serde_json::to_value(&p).unwrap_or_default();
    product["price"] = json!(money(p.price_minor, &p.currency));
    if let Some(variants) = product["variants"].as_array_mut() {
        for (value, variant) in variants.iter_mut().zip(&p.variants) {
            value["price"] = json!(money(variant.price_minor, &variant.currency));
        }
    }
    v.set("page_title", json!(p.title));
    let summary: String = p.description.chars().take(160).collect();
    if !summary.is_empty() {
        v.set("meta_description", json!(summary));
    }
    v.set("canonical", json!(format!("/products/{}", p.handle)));
    v.set("product", product);
    v.render("product", StatusCode::OK).await
}

/// Sets `products`, `prev_url` and `next_url` from a page of cards.
fn list(v: &mut View, page: &Paged<ProductCard>, base: &str) {
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
    let Some(c) = catalog::collection_by_handle(&mut v.tx, &shop, &handle).await? else {
        return v.not_found().await;
    };
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
    v.set("page_title", json!(c.title));
    v.set("canonical", json!(format!("/collections/{}", c.handle)));
    list(&mut v, &page, &format!("/collections/{}", c.handle));
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
    let Some(p) = soat_cms::published_page(&mut v.tx, &shop, &handle).await? else {
        return v.not_found().await;
    };
    let b = blocks(&mut v.tx, &shop, &storage, &p.blocks).await?;
    v.set(
        "page_title",
        json!(p.seo_title.clone().unwrap_or_else(|| p.title.clone())),
    );
    if let Some(d) = &p.seo_description {
        v.set("meta_description", json!(d));
    }
    v.set("canonical", json!(format!("/pages/{}", p.handle)));
    v.set("blocks", Value::Array(b));
    v.set("page", serde_json::to_value(&p).unwrap_or_default());
    v.render("page", StatusCode::OK).await
}

/// Search, or every product without `?q=`.
#[get("/search", tag = "storefront")]
async fn search_page(
    db: Db,
    shop: Shop,
    storage: Storage,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let q: String = params
        .get("q")
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    let query = format!("q={}&offset={}", encode(&q), offset(&params));
    let page =
        catalog::product_cards(&mut v.tx, &shop, &storage, &ListParams::from_query(&query)).await?;
    v.set(
        "page_title",
        json!(if q.is_empty() {
            "All products".to_owned()
        } else {
            format!("Search: {q}")
        }),
    );
    list(&mut v, &page, &format!("/search?q={}", encode(&q)));
    v.set("query", json!(q));
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
    v.set("page_title", json!("Your cart"));
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

fn cart_link(count: Option<i64>, notice: Option<&str>) -> Response {
    let count = count.map(|n| format!(" ({n})")).unwrap_or_default();
    let mut out =
        format!("<a class=\"cart-link\" href=\"/cart\" id=\"cart-count\">Cart{count}</a>");
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
    let cookie = format!(
        "{CART_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000{}",
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
    let currency = product.currency.clone();
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
        (Ok(cart), true) => cart_link(Some(cart.item_count), None),
        (Ok(_), false) => Redirect::to("/cart").into_response(),
        (Err(e), true) => cart_link(None, Some(&e.to_string())),
        (Err(_), false) => return outcome.map(|_| StatusCode::OK.into_response()),
    };
    if outcome.is_ok() {
        if let Some(c) = cookie {
            set_cookie(&mut res, &c, conn.secure);
        }
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
        return Ok(Redirect::to("/cart").into_response());
    };
    let holder = CartToken::new(shop.clone(), &token);
    let (mut tx, id) = holder.open(&db, &id).await?;
    carts::set_quantity(&mut tx, &shop, &id, &line_id, f.quantity.clamp(0, 999)).await?;
    tx.commit().await?;
    if is_htmx(&headers) {
        render_cart(&db, &shop, &headers).await
    } else {
        Ok(Redirect::to("/cart").into_response())
    }
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
    }
}
