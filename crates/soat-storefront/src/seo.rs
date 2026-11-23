//! What search engines read: `robots.txt`, `sitemap.xml`, and themed
//! not-found pages that follow the shop's redirects.

use soat_catalog::storefront as catalog;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use super::{origin, View};

/// Paths buyers reach only through the shop, kept out of search results.
const PRIVATE: &[&str] = &["/cart", "/checkout", "/order", "/account", "/search"];

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn text(body: String, content_type: &'static str) -> Response {
    let mut res = body.into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    res
}

/// The shop's `robots.txt`: everything but the cart, checkout, account
/// and search, and where the sitemap is.
#[get("/robots.txt", tag = "storefront")]
pub(crate) async fn robots(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut tx = shop.begin(&db).await?;
    let base = origin(&mut tx, &shop, &headers).await?;
    tx.commit().await?;
    let mut out = String::from("User-agent: *\n");
    for p in PRIVATE {
        out.push_str(&format!("Disallow: {p}\n"));
    }
    out.push_str(&format!("\nSitemap: {base}/sitemap.xml\n"));
    Ok(text(out, "text/plain; charset=utf-8"))
}

/// The shop's sitemap: the home page, published pages, active products and
/// collections.
#[get("/sitemap.xml", tag = "storefront")]
pub(crate) async fn sitemap(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut tx = shop.begin(&db).await?;
    let base = origin(&mut tx, &shop, &headers).await?;
    let pages = soat_cms::published_handles(&mut tx, &shop).await?;
    let (products, collections) = catalog::sitemap_handles(&mut tx, &shop).await?;
    tx.commit().await?;
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    let mut url = |path: String| {
        out.push_str(&format!(
            "<url><loc>{}</loc></url>\n",
            xml_escape(&format!("{base}{path}"))
        ));
    };
    url("/".to_owned());
    for h in pages.iter().filter(|h| *h != "home") {
        url(format!("/pages/{h}"));
    }
    for h in &collections {
        url(format!("/collections/{h}"));
    }
    for h in &products {
        url(format!("/products/{h}"));
    }
    out.push_str("</urlset>\n");
    Ok(text(out, "application/xml; charset=utf-8"))
}

/// Requests that match no route: on the shop, a redirect when the shop has
/// one for the path, else the theme's not-found page; JSON 404 for the API.
pub(crate) async fn fallback(
    db: Db,
    shop: Option<Shop>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
) -> Result<Response> {
    let path = uri.path();
    let api = path == "/api" || path.starts_with("/api/") || path.starts_with("/mcp");
    let (Some(shop), false, true) = (shop, api, method == Method::GET || method == Method::HEAD)
    else {
        return Err(Error::not_found());
    };
    let v = View::open(&db, &shop, &headers).await?;
    v.missing(&shop, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_xml() {
        assert_eq!(
            xml_escape("/a?b=1&c=<'\">"),
            "/a?b=1&amp;c=&lt;&apos;&quot;&gt;"
        );
    }
}
