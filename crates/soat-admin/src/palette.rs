//! The command palette: Ctrl+K (or Cmd+K, or `/`) finds a page, product,
//! order or customer and goes there. Results come from the API as the
//! staff member, so they only find what their role may read.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::encode;
use crate::{Ctx, Shared};

/// Pages to go to, with words that find them.
const PLACES: &[(&str, &str, &str)] = &[
    ("Home", "", "dashboard start"),
    ("Orders", "/orders", "sales"),
    (
        "Orders to ship",
        "/orders?view=unfulfilled",
        "fulfil fulfill shipping",
    ),
    ("Unpaid orders", "/orders?view=unpaid", "payments"),
    ("Products", "/products", "catalog"),
    ("New product", "/products/new", "add create"),
    ("Customers", "/customers", "buyers"),
    ("New customer", "/customers/new", "add create"),
    ("Pages", "/pages", "content cms"),
    ("Menus", "/menus", "navigation"),
    ("Redirects", "/redirects", "urls"),
    ("Theme", "/theme", "design look"),
    ("Assistant", "/assistant", "claude ai chat"),
    ("Build your shop", "/build", "claude ai draft"),
    ("Import and export", "/transfer", "csv shopify"),
    ("Launch checklist", "/launch", "go live"),
    ("Welcome steps", "/welcome", "setup onboarding"),
    ("Settings", "/settings", "configuration"),
    ("General settings", "/settings/general", "name domain"),
    ("Payments", "/settings/payments", "stripe mollie paypal"),
    ("Shipping", "/settings/shipping", "rates zones"),
    ("Taxes", "/settings/taxes", "vat"),
    ("Email", "/settings/email", "templates notifications"),
    ("Invoices", "/settings/invoices", "legal"),
    ("Staff", "/settings/staff", "roles team"),
    ("Developers", "/settings/developers", "api keys webhooks"),
    (
        "Assistant settings",
        "/settings/assistant",
        "anthropic key approval",
    ),
];

/// Places whose name or words contain every word of `q`.
pub(crate) fn places(base: &str, q: &str) -> Vec<Value> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    PLACES
        .iter()
        .filter(|(label, _, more)| {
            let hay = format!("{} {more}", label.to_lowercase());
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .take(8)
        .map(|(label, path, _)| json!({ "kind": "Go to", "label": label, "href": format!("{base}{path}") }))
        .collect()
}

/// What matches `q`: places, then products, orders and customers.
#[get("/admin/{shop}/palette", tag = "admin")]
pub(crate) async fn search(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<serde_json::Map<String, Value>>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let base = format!("/admin/{shop}");
    let text: String = q
        .get("q")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .chars()
        .take(100)
        .collect();
    let mut results = places(&base, &text);
    let query = text.trim_start_matches('#');
    if !query.is_empty() {
        let found = |path: String| {
            let cx = &cx;
            async move {
                let (status, out) = cx.call(Method::GET, &path, None).await;
                if status == 200 {
                    out["items"].as_array().cloned().unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
        };
        let q = encode(query);
        for p in found(format!("/api/products?q={q}&limit=5")).await {
            results.push(json!({
                "kind": "Product", "label": p["title"], "hint": p["sku"],
                "href": format!("{base}/products/{}", p["id"].as_str().unwrap_or_default()),
            }));
        }
        for o in found(format!("/api/orders?q={q}&limit=5")).await {
            results.push(json!({
                "kind": "Order", "label": format!("#{}", o["number"]), "hint": o["email"],
                "href": format!("{base}/orders/{}", o["id"].as_str().unwrap_or_default()),
            }));
        }
        for c in found(format!("/api/customers?q={q}&limit=5")).await {
            let name = format!(
                "{} {}",
                c["first_name"].as_str().unwrap_or_default(),
                c["last_name"].as_str().unwrap_or_default()
            );
            let name = name.trim();
            results.push(json!({
                "kind": "Customer",
                "label": if name.is_empty() { c["email"].clone() } else { json!(name) },
                "hint": c["email"],
                "href": format!("{base}/customers/{}", c["id"].as_str().unwrap_or_default()),
            }));
        }
    }
    Ok(cx
        .s
        .pages
        .fragment("palette", &json!({ "results": results })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_match_every_word() {
        let all = places("/admin/acme", "");
        assert_eq!(all.len(), 8);
        let vat = places("/admin/acme", "VAT");
        assert_eq!(vat[0]["href"], "/admin/acme/settings/taxes");
        assert!(places("/admin/acme", "orders ship")[0]["label"] == "Orders to ship");
        assert!(places("/admin/acme", "nothing like this").is_empty());
    }
}
