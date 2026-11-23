//! The shop builder: the owner describes the shop, Claude drafts products,
//! collections and pages, the owner reviews the draft and builds it.
//! Building goes through the API as the staff member, like any form here.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::samples::{call, create_product, Failed};
use crate::settings::Fields;
use crate::{Ctx, Shared};

/// The shop's currency, `EUR` until it chose one.
async fn currency(cx: &Ctx) -> String {
    let (_, shop) = cx.call(Method::GET, "/api/shop", None).await;
    shop["currency"].as_str().unwrap_or("EUR").to_owned()
}

fn render(mut cx: Ctx, description: &str, plan: Option<Value>, error: Option<String>) -> Response {
    cx.set("description", json!(description));
    if let Some(p) = plan {
        cx.set("plan_json", json!(p.to_string()));
        cx.set("plan", p);
    }
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Build your shop"));
    cx.render("builder", StatusCode::OK)
}

/// Describe the shop; Claude drafts it.
#[get("/admin/{shop}/build", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    Ok(render(cx, "", None, None))
}

/// Draft the shop from its description (a form post), shown for review.
#[post("/admin/{shop}/build", tag = "admin")]
pub(crate) async fn draft(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let description = Fields::parse(&body).get("description");
    let body = json!({ "description": description, "currency": currency(&cx).await });
    let (status, out) = cx
        .call(Method::POST, "/api/assistant/plan", Some(&body))
        .await;
    Ok(match status {
        200 => {
            let plan = with_prices(out, &currency(&cx).await);
            render(cx, &description, Some(plan), None)
        }
        401 => crate::to_login(),
        _ => {
            let e = crate::assistant::problem(status, &out);
            render(cx, &description, None, Some(e))
        }
    })
}

/// Adds the currency to each product's price, for the preview.
fn with_prices(mut plan: Value, currency: &str) -> Value {
    if let Some(products) = plan["products"].as_array_mut() {
        for p in products {
            p["shown_price"] = json!(format!("{} {currency}", p["price"].as_str().unwrap_or("")));
            p["variants"] = json!(p["option"]["values"]
                .as_array()
                .map(|v| v
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default());
        }
    }
    plan
}

/// Build the reviewed draft (a form post): its products (active, with
/// variants), collections and pages (unpublished, to read first). A SKU,
/// collection or page handle the shop has already is left as it is.
#[post("/admin/{shop}/build/apply", tag = "admin")]
pub(crate) async fn apply(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let f = Fields::parse(&body);
    let Ok(plan) = serde_json::from_str::<Value>(&f.get("plan")) else {
        return Ok(render(
            cx,
            "",
            None,
            Some("The draft was lost. Describe the shop again.".into()),
        ));
    };
    let currency = currency(&cx).await;
    Ok(match build(&cx, &plan, &currency).await {
        Ok(()) => Redirect::to(&format!("/admin/{shop}/products")).into_response(),
        Err((401, _)) => crate::to_login(),
        Err((status, out)) => cx.failed(status, &out),
    })
}

async fn build(cx: &Ctx, plan: &Value, currency: &str) -> Result<(), Failed> {
    let mut ids: Vec<(String, String)> = Vec::new();
    for p in plan["products"].as_array().into_iter().flatten().take(24) {
        if let Some(id) = create_product(cx, p, currency, None, None).await? {
            ids.push((p["sku"].as_str().unwrap_or_default().to_owned(), id));
        }
    }
    for c in plan["collections"].as_array().into_iter().flatten().take(6) {
        let body = json!({ "title": c["title"], "description": c["description"] });
        let collection = match call(cx, Method::POST, "/api/collections", Some(&body)).await {
            Ok(c) => c["id"].as_str().unwrap_or_default().to_owned(),
            Err((409, _)) => continue,
            Err(e) => return Err(e),
        };
        for sku in c["skus"].as_array().into_iter().flatten() {
            if let Some((_, id)) = ids.iter().find(|(s, _)| sku == s.as_str()) {
                let path = format!("/api/collections/{collection}/products/{id}");
                call(cx, Method::PUT, &path, None).await?;
            }
        }
    }
    for p in plan["pages"].as_array().into_iter().flatten().take(6) {
        let mut body = json!({
            "title": p["title"], "published": false,
            "blocks": [{ "type": "text", "body": p["body"] }],
        });
        let handle = p["handle"].as_str().unwrap_or_default();
        if !handle.is_empty() {
            body["handle"] = json!(handle);
        }
        match call(cx, Method::POST, "/api/pages", Some(&body)).await {
            Ok(_) | Err((409, _)) => {}
            // A handle the API does not take: let it choose one.
            Err((422, _)) if !handle.is_empty() => {
                body.as_object_mut().map(|b| b.remove("handle"));
                match call(cx, Method::POST, "/api/pages", Some(&body)).await {
                    Ok(_) | Err((409, _)) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
