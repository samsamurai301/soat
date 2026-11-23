//! The storefront in the shop's languages and markets. Addresses in the default
//! language have no prefix; others start with the language, e.g.
//! `/de/products/trail-shoe`. A global middleware takes the prefix off
//! before routing, so every page and form works in every language.

use std::collections::HashMap;

use serde_json::{Map, Value};
use soat_catalog::storefront::{ProductCard, ShopCollection, ShopProduct};
use soat_cms::{Menu, Page};
use soat_core::i18n::{is_language_code, language_name};
use soat_core::Currency;
use soat_i18n::{translated, ShopLanguages};
use soat_pricing::Market;
use soat_tenancy::Scope;
use zapline::db::sqlx::PgConnection;
use zapline::prelude::*;

/// The language an address asked for; only the storefront's middleware
/// sets it.
pub const LANGUAGE_HEADER: &str = "x-soat-language";
/// The address without its language prefix, for language links.
pub const PATH_HEADER: &str = "x-soat-path";

/// `/de/x` → `("de", "/x")`; `/de` → `("de", "/")`.
fn split(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix('/')?;
    let (code, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    is_language_code(code).then_some((code, tail))
}

/// Takes a language prefix off the address before routing.
pub(crate) async fn language_prefix(mut req: Request, next: Next) -> Response {
    req.headers_mut().remove(LANGUAGE_HEADER);
    req.headers_mut().remove(PATH_HEADER);
    let uri = req.uri().clone();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let (lang, path) = match split(uri.path()) {
        Some((lang, rest)) => (Some(lang.to_owned()), rest.to_owned()),
        None => (None, uri.path().to_owned()),
    };
    if let Ok(v) = HeaderValue::from_str(&format!("{path}{query}")) {
        req.headers_mut().insert(PATH_HEADER, v);
    }
    if let Some(lang) = lang {
        if let (Ok(new), Ok(v)) = (
            format!("{path}{query}").parse::<Uri>(),
            HeaderValue::from_str(&lang),
        ) {
            req.set_uri(new);
            req.headers_mut().insert(LANGUAGE_HEADER, v);
        }
    }
    next.run(req).await
}

/// The prefix for the request's language (`/de`), or none; for redirects.
pub(crate) fn root_of(headers: &HeaderMap) -> String {
    headers
        .get(LANGUAGE_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|l| is_language_code(l))
        .map(|l| format!("/{l}"))
        .unwrap_or_default()
}

/// The language a page is shown in.
pub(crate) struct Lang {
    /// e.g. `de`.
    pub code: String,
    /// `/de`, or empty in the default language.
    pub root: String,
    pub shop: ShopLanguages,
    /// The buyer's market, from their country; prices show in its
    /// currency.
    pub market: Option<Market>,
}

impl Lang {
    /// The asked-for language when the shop publishes it, else the
    /// default.
    pub fn of(headers: &HeaderMap, shop: ShopLanguages) -> Self {
        let asked = headers
            .get(LANGUAGE_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|l| shop.has(l))
            .map(str::to_owned);
        match asked {
            Some(code) => Lang {
                root: format!("/{code}"),
                code,
                shop,
                market: None,
            },
            None => Lang {
                code: shop.default_language.clone(),
                root: String::new(),
                shop,
                market: None,
            },
        }
    }

    /// Whether texts need translating.
    pub fn translated(&self) -> bool {
        !self.root.is_empty()
    }

    /// The market's currency when it is not `own`.
    fn currency(&self, own: &str) -> Option<Currency> {
        self.market
            .as_ref()
            .filter(|m| m.currency != own)
            .and_then(|m| Currency::parse(&m.currency).ok())
    }

    /// `path` in this language.
    pub fn url(&self, path: &str) -> String {
        prefixed(&self.root, path)
    }

    /// Links to `path` in each language, for a switcher; none for a shop
    /// with one language.
    pub fn switcher(&self, path: &str) -> Vec<Value> {
        if self.shop.languages.is_empty() {
            return Vec::new();
        }
        self.shop
            .all()
            .into_iter()
            .map(|code| {
                let root = if code == self.shop.default_language {
                    String::new()
                } else {
                    format!("/{code}")
                };
                serde_json::json!({
                    "href": prefixed(&root, path),
                    "name": language_name(&code),
                    "current": code == self.code,
                    "code": code,
                })
            })
            .collect()
    }

    /// `hreflang` alternates of `path` on `origin`, with `x-default`.
    pub fn alternates(&self, origin: &str, path: &str) -> Vec<Value> {
        if self.shop.languages.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Value> = self
            .shop
            .all()
            .into_iter()
            .map(|code| {
                let root = if code == self.shop.default_language {
                    String::new()
                } else {
                    format!("/{code}")
                };
                serde_json::json!({ "hreflang": code, "href": format!("{origin}{}", prefixed(&root, path)) })
            })
            .collect();
        out.push(serde_json::json!({ "hreflang": "x-default", "href": format!("{origin}{path}") }));
        out
    }
}

/// The cookie holding the buyer's country, e.g. `CH`.
pub const COUNTRY_COOKIE: &str = "soat_country";

/// The buyer's market, from the country they picked.
pub(crate) async fn market_of(
    conn: &mut PgConnection,
    scope: &impl Scope,
    headers: &HeaderMap,
) -> Result<Option<Market>> {
    match super::cookie(headers, COUNTRY_COOKIE) {
        Some(c) if c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()) => {
            soat_pricing::market_for(conn, scope, &c).await
        }
        _ => Ok(None),
    }
}

/// The countries of the shop's markets for a picker, by name; none
/// without markets.
pub(crate) async fn countries(
    conn: &mut PgConnection,
    scope: &impl Scope,
    current: Option<&str>,
) -> Result<Vec<Value>> {
    let mut out: Vec<(&'static str, String, String)> = Vec::new();
    for m in soat_pricing::active_markets(conn, scope).await? {
        for c in m.countries {
            if let Some(name) = soat_core::address::country_name(&c) {
                out.push((name, c, m.currency.clone()));
            }
        }
    }
    out.sort();
    Ok(out
        .into_iter()
        .map(|(name, code, currency)| {
            serde_json::json!({
                "code": code,
                "name": name,
                "currency": currency,
                "selected": Some(code.as_str()) == current,
            })
        })
        .collect())
}

/// `path` under `root` when it is a path on the shop.
pub(crate) fn prefixed(root: &str, path: &str) -> String {
    if root.is_empty() || !path.starts_with('/') || path.starts_with("//") {
        path.to_owned()
    } else if path == "/" {
        root.to_owned()
    } else {
        format!("{root}{path}")
    }
}

/// Puts the language prefix on the `url` of links (menu items, buttons,
/// image links), nested lists included.
pub(crate) fn prefix_links(root: &str, v: &mut Value) {
    if root.is_empty() {
        return;
    }
    match v {
        Value::Array(items) => items.iter_mut().for_each(|i| prefix_links(root, i)),
        Value::Object(map) => {
            for key in ["url", "link"] {
                if let Some(Value::String(u)) = map.get(key) {
                    let new = prefixed(root, u);
                    map.insert(key.to_owned(), Value::String(new));
                }
            }
            for key in ["items", "blocks", "columns"] {
                if let Some(inner) = map.get_mut(key) {
                    prefix_links(root, inner);
                }
            }
        }
        _ => {}
    }
}

fn take(fields: &mut HashMap<String, String>, name: &str, into: &mut String) {
    if let Some(v) = fields.remove(name) {
        *into = v;
    }
}

/// `minor` of `own` in the market's currency, when the item is sold in it.
async fn reprice(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    item_id: &str,
    minor: &mut i64,
    currency: &mut String,
) -> Result<()> {
    let (Some(to), Ok(own)) = (lang.currency(currency), Currency::parse(currency)) else {
        return Ok(());
    };
    if let Some(p) = soat_pricing::shown_price(conn, scope, item_id, *minor, own, to).await? {
        *minor = p;
        *currency = to.as_str().to_owned();
    }
    Ok(())
}

/// Product cards' titles in `lang`, prices in the market's currency.
pub(crate) async fn cards(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    cards: &mut [ProductCard],
) -> Result<()> {
    for c in cards.iter_mut() {
        reprice(
            conn,
            scope,
            lang,
            &c.id,
            &mut c.price_minor,
            &mut c.currency,
        )
        .await?;
    }
    if !lang.translated() || cards.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = cards.iter().map(|c| c.id.clone()).collect();
    let mut all = translated(conn, scope, "product", &ids, &lang.code).await?;
    for c in cards {
        if let Some(f) = all.get_mut(&c.id) {
            take(f, "title", &mut c.title);
        }
    }
    Ok(())
}

/// A product's texts in `lang`, prices in the market's currency.
pub(crate) async fn product(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    p: &mut ShopProduct,
) -> Result<()> {
    reprice(
        conn,
        scope,
        lang,
        &p.id,
        &mut p.price_minor,
        &mut p.currency,
    )
    .await?;
    for v in p.variants.iter_mut() {
        reprice(
            conn,
            scope,
            lang,
            &v.id,
            &mut v.price_minor,
            &mut v.currency,
        )
        .await?;
    }
    if !lang.translated() {
        return Ok(());
    }
    let mut all = translated(
        conn,
        scope,
        "product",
        std::slice::from_ref(&p.id),
        &lang.code,
    )
    .await?;
    if let Some(mut f) = all.remove(&p.id) {
        take(&mut f, "title", &mut p.title);
        take(&mut f, "description", &mut p.description);
        take(&mut f, "seo_title", &mut p.seo_title);
        take(&mut f, "seo_description", &mut p.seo_description);
    }
    Ok(())
}

/// A collection's texts in `lang`.
pub(crate) async fn collection(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    c: &mut ShopCollection,
) -> Result<()> {
    if !lang.translated() {
        return Ok(());
    }
    let mut all = translated(
        conn,
        scope,
        "collection",
        std::slice::from_ref(&c.id),
        &lang.code,
    )
    .await?;
    if let Some(mut f) = all.remove(&c.id) {
        take(&mut f, "title", &mut c.title);
        take(&mut f, "description", &mut c.description);
        take(&mut f, "seo_title", &mut c.seo_title);
        take(&mut f, "seo_description", &mut c.seo_description);
    }
    Ok(())
}

/// A content page's texts and blocks in `lang`.
pub(crate) async fn page(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    p: &mut Page,
) -> Result<()> {
    if !lang.translated() {
        return Ok(());
    }
    let mut all = translated(conn, scope, "page", std::slice::from_ref(&p.id), &lang.code).await?;
    if let Some(mut f) = all.remove(&p.id) {
        take(&mut f, "title", &mut p.title);
        if let Some(t) = f.remove("seo_title") {
            p.seo_title = Some(t);
        }
        if let Some(d) = f.remove("seo_description") {
            p.seo_description = Some(d);
        }
        if let Some(b) = f
            .remove("blocks")
            .and_then(|b| serde_json::from_str(&b).ok())
        {
            p.blocks = b;
        }
    }
    Ok(())
}

/// A menu's items in `lang`, links prefixed.
pub(crate) async fn menu(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
    m: &Menu,
) -> Result<Value> {
    let mut items = Value::Array(m.items.clone());
    if lang.translated() {
        let mut all =
            translated(conn, scope, "menu", std::slice::from_ref(&m.id), &lang.code).await?;
        if let Some(i) = all
            .remove(&m.id)
            .and_then(|mut f| f.remove("items"))
            .and_then(|i| serde_json::from_str::<Value>(&i).ok())
        {
            items = i;
        }
    }
    prefix_links(&lang.root, &mut items);
    Ok(items)
}

/// The theme's texts in `lang`: the default theme's, then the shop's own.
pub(crate) async fn texts(
    conn: &mut PgConnection,
    scope: &impl Scope,
    lang: &Lang,
) -> Result<Map<String, Value>> {
    let mut t = soat_theme::strings(&lang.code);
    if lang.translated() {
        for (k, v) in soat_i18n::theme_strings(conn, scope, &lang.code).await? {
            t.insert(k, Value::String(v));
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(split("/de/products/x"), Some(("de", "/products/x")));
        assert_eq!(split("/de-at"), Some(("de-at", "/")));
        assert_eq!(split("/de/"), Some(("de", "/")));
        assert_eq!(split("/api/x"), None);
        assert_eq!(split("/cart"), None);
        assert_eq!(prefixed("/de", "/"), "/de");
        assert_eq!(prefixed("/de", "/cart"), "/de/cart");
        assert_eq!(prefixed("/de", "https://x"), "https://x");
        assert_eq!(prefixed("/de", "//evil"), "//evil");
        assert_eq!(prefixed("", "/cart"), "/cart");
        let mut v = serde_json::json!([{ "url": "/a", "items": [{ "url": "https://b" }, { "url": "/c" }] }]);
        prefix_links("/de", &mut v);
        assert_eq!(
            v,
            serde_json::json!([{ "url": "/de/a", "items": [{ "url": "https://b" }, { "url": "/de/c" }] }])
        );
    }
}
