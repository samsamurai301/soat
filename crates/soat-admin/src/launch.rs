//! The launch checklist: what a shop needs before it takes real orders,
//! each with where to fix it. It reads the API only; nothing is stored.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::{Ctx, Shared};

/// The legal pages an EU shop needs, with the handles each may have.
const LEGAL: [(&str, &str, &str, &[&str]); 4] = [
    (
        "imprint",
        "Imprint",
        "Who runs the shop and how to reach them (Impressum).",
        &["imprint", "impressum", "legal-notice", "mentions-legales"],
    ),
    (
        "privacy",
        "Privacy policy",
        "What personal data the shop keeps and why (GDPR).",
        &[
            "privacy",
            "privacy-policy",
            "datenschutz",
            "datenschutzerklaerung",
        ],
    ),
    (
        "terms",
        "Terms and conditions",
        "The terms every order is placed under.",
        &["terms", "terms-and-conditions", "agb", "tos"],
    ),
    (
        "withdrawal",
        "Right of withdrawal",
        "How consumers cancel within 14 days, with the model form.",
        &[
            "withdrawal",
            "right-of-withdrawal",
            "widerruf",
            "widerrufsbelehrung",
            "returns",
        ],
    ),
];

/// One check: whether it is done, why it matters and where to fix it.
fn item(key: &str, label: &str, why: &str, done: bool, fix: String, needed: bool) -> Value {
    json!({
        "key": key, "label": label, "why": why, "done": done, "fix": fix, "needed": needed,
        "post": false,
    })
}

/// Every check for the shop, from what the API shows. A check the staff
/// member's role cannot read counts as not done.
pub(crate) async fn checks(cx: &Ctx) -> Vec<Value> {
    let base = cx.data["base"].as_str().unwrap_or_default().to_owned();
    let get = |path: String| async move {
        let (status, out) = cx.call(Method::GET, &path, None).await;
        if status == 200 {
            out
        } else {
            Value::Null
        }
    };
    let items = |v: &Value| {
        v["items"]
            .as_array()
            .cloned()
            .or_else(|| v.as_array().cloned())
            .unwrap_or_default()
    };
    let shop = get("/api/shop".into()).await;
    let products = get("/api/products?status=active&limit=1".into()).await;
    let methods = items(&get("/api/payment-methods".into()).await);
    let shipping = items(&get("/api/shipping/zones".into()).await);
    let taxes = items(&get("/api/tax/zones".into()).await);
    let pages = items(&get("/api/pages?limit=200".into()).await);
    let domains = items(&get("/api/domains".into()).await);
    let invoices = get("/api/invoices/settings".into()).await;
    let email = get("/api/notifications/settings".into()).await;

    let mut out = vec![
        item(
            "basics",
            "Country and currency",
            "Taxes, shipping and new prices start from them.",
            shop["country"].is_string() && shop["currency"].is_string(),
            format!("{base}/welcome#basics"),
            true,
        ),
        item(
            "products",
            "An active product",
            "Drafts and archived products are not for sale.",
            products["total"].as_i64().unwrap_or(0) > 0,
            format!("{base}/products"),
            true,
        ),
        item(
            "payments",
            "A way to pay",
            "Checkout needs at least one active payment method.",
            methods.iter().any(|m| m["active"] != json!(false)),
            format!("{base}/settings/payments"),
            true,
        ),
        item(
            "shipping",
            "Shipping rates",
            "Orders with physical goods need a rate for the buyer's country.",
            shipping
                .iter()
                .any(|z| z["rates"].as_array().is_some_and(|r| !r.is_empty())),
            format!("{base}/settings/shipping"),
            true,
        ),
        item(
            "taxes",
            "Tax rates",
            "Without a tax zone for a country, its orders carry no tax.",
            !taxes.is_empty(),
            format!("{base}/settings/taxes"),
            true,
        ),
    ];
    for (key, label, why, handles) in LEGAL {
        let page = pages
            .iter()
            .find(|p| p["handle"].as_str().is_some_and(|h| handles.contains(&h)));
        let done = page.is_some_and(|p| p["published"] == json!(true));
        let fix = match page {
            Some(p) => format!("{base}/pages/{}", p["id"].as_str().unwrap_or_default()),
            None => format!("{base}/launch/page/{key}"),
        };
        let mut check = item(key, label, why, done, fix, true);
        check["post"] = json!(page.is_none());
        out.push(check);
    }
    let legal = &invoices["legal_name"];
    out.push(item(
        "invoices",
        "Legal details on invoices",
        "Invoices must name the seller, its address and VAT number.",
        legal.as_str().is_some_and(|l| !l.is_empty()),
        format!("{base}/settings/invoices"),
        false,
    ));
    out.push(item(
        "email",
        "A reply-to address",
        "Customers answer order emails; replies should reach you.",
        email["reply_to"].is_string(),
        format!("{base}/settings/email"),
        false,
    ));
    out.push(item(
        "domain",
        "Your own domain",
        "Customers find and trust a shop on its own address.",
        !domains.is_empty(),
        format!("{base}/settings/general"),
        false,
    ));
    out
}

/// What the shop still needs before it takes real orders.
#[get("/admin/{shop}/launch", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let all = checks(&cx).await;
    let (needed, recommended): (Vec<Value>, Vec<Value>) =
        all.into_iter().partition(|c| c["needed"] == json!(true));
    let open = needed.iter().filter(|c| c["done"] != json!(true)).count();
    cx.set("needed", json!(needed));
    cx.set("recommended", json!(recommended));
    cx.set("open", json!(open));
    cx.set("ready", json!(open == 0));
    cx.set("page_title", json!("Launch checklist"));
    Ok(cx.render("launch", StatusCode::OK))
}

/// Start a missing legal page (a form post): a draft with headings to fill
/// in, opened for editing. It is not legal advice, and says so.
#[post("/admin/{shop}/launch/page/{key}", tag = "admin")]
pub(crate) async fn start_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, key)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let Some((_, label, why, handles)) = LEGAL.iter().find(|(k, ..)| *k == key) else {
        return Ok(cx.failed(404, &Value::Null));
    };
    let body = json!({
        "title": label, "handle": handles[0], "published": false,
        "blocks": [
            { "type": "heading", "text": label, "level": 1 },
            { "type": "text", "body": format!(
                "{why}\n\nWrite this page for your shop and your country, then publish it. \
                 SOAT does not give legal advice; have it checked where you are unsure."
            ) },
        ],
    });
    let (status, out) = cx.call(Method::POST, "/api/pages", Some(&body)).await;
    Ok(match status {
        201 => Redirect::to(&format!(
            "/admin/{shop}/pages/{}",
            out["id"].as_str().unwrap_or_default()
        ))
        .into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}
