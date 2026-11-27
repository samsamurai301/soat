//! Subscriptions: the plans products are sold on again and again, and
//! the customers who subscribed, with pause, resume, skip and cancel.

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// A subscription with what the page shows of its status.
fn shown(s: &Value) -> Value {
    let mut s = s.clone();
    let status = s["status"].as_str().unwrap_or_default().to_owned();
    s["active"] = json!(status == "active");
    s["paused"] = json!(status == "paused");
    s["canceled"] = json!(status == "canceled");
    s["next_day"] = json!(s["next_order_at"].as_str().unwrap_or_default().get(..10));
    s
}

/// The subscriptions page's query.
#[model]
pub struct SubscriptionsQuery {
    /// active, paused or canceled; all when left out.
    pub status: Option<String>,
}

/// The shop's subscriptions and plans, with a form for a new plan.
#[get("/admin/{shop}/subscriptions", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<SubscriptionsQuery>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    Ok(page(cx, q.status, None, Value::Null, StatusCode::OK).await)
}

async fn page(
    mut cx: Ctx,
    status: Option<String>,
    error: Option<String>,
    typed: Value,
    code: StatusCode,
) -> Response {
    let filter = status
        .as_deref()
        .filter(|s| ["active", "paused", "canceled"].contains(s));
    let path = match filter {
        Some(f) => format!("/api/subscriptions?status={f}"),
        None => "/api/subscriptions".to_owned(),
    };
    let (got, subs) = cx.call(Method::GET, &path, None).await;
    match got {
        200 => {}
        404 => {
            cx.set("missing", json!(true));
            cx.set("page_title", json!("Subscriptions"));
            return cx.render("subscriptions", StatusCode::OK);
        }
        _ => return cx.failed(got, &subs),
    }
    let (got, plans) = cx.call(Method::GET, "/api/subscription-plans", None).await;
    if got != 200 {
        return cx.failed(got, &plans);
    }
    let (got, products) = cx
        .call(Method::GET, "/api/products?status=active&limit=200", None)
        .await;
    if got != 200 {
        return cx.failed(got, &products);
    }
    let product = |id: &str| {
        products["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"] == id)
            .cloned()
            .unwrap_or_else(|| json!({ "title": id }))
    };
    let plans: Vec<Value> = plans["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let mut p = p.clone();
            let of = product(p["product_id"].as_str().unwrap_or_default());
            p["product_title"] = of["title"].clone();
            let unit = p["interval_unit"].as_str().unwrap_or("month").to_owned();
            let plural = if p["interval_count"] == 1 { "" } else { "s" };
            p["unit_label"] = json!(format!("{unit}{plural}"));
            let bps = p["discount_bps"].as_i64().unwrap_or(0);
            p["discount_percent"] = json!(format!("{}.{:02}", bps / 100, bps % 100));
            let ship = p["shipping_price_minor"].as_i64().unwrap_or(0);
            p["shipping"] = json!(
                match Currency::parse(of["currency"].as_str().unwrap_or("")) {
                    Ok(c) => c.format(ship),
                    Err(_) => ship.to_string(),
                }
            );
            p
        })
        .collect();
    let items: Vec<Value> = subs["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(shown)
        .collect();
    cx.set("subscriptions", Value::Array(items));
    cx.set("plans", Value::Array(plans));
    cx.set("products", products["items"].clone());
    cx.set("status", json!(filter));
    cx.set("typed", typed);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Subscriptions"));
    cx.render("subscriptions", code)
}

/// Add a plan to a product (a form post).
#[post("/admin/{shop}/subscription-plans", tag = "admin")]
pub(crate) async fn create_plan(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    // The shipping price is in the product's currency; the discount is a
    // percentage with up to two decimals, so basis points.
    let product_id = f.get("product_id");
    let (_, of) = cx
        .call(
            Method::GET,
            &format!("/api/products/{}", encode(&product_id)),
            None,
        )
        .await;
    let currency = Currency::parse(of["currency"].as_str().unwrap_or("EUR"))
        .or_else(|_| Currency::parse("EUR"))
        .map_err(|_| Error::internal("EUR is a currency"))?;
    let two = Currency::parse("EUR").map_err(|_| Error::internal("EUR is a currency"))?;
    let number = |c: &Currency, k: &str| {
        let t = f.get(k);
        let t = t.trim();
        if t.is_empty() {
            None
        } else {
            Some(c.parse_decimal(t).unwrap_or(-1))
        }
    };
    let body = json!({
        "product_id": product_id,
        "name": f.get("name"),
        "interval_unit": f.get("interval_unit"),
        "interval_count": f.get("interval_count").trim().parse::<i64>().unwrap_or(0),
        "discount_bps": number(&two, "discount_percent"),
        "shipping_price_minor": number(&currency, "shipping_price"),
    });
    let (status, out) = cx
        .call(Method::POST, "/api/subscription-plans", Some(&body))
        .await;
    Ok(match status {
        201 => Redirect::to(&format!("/admin/{shop}/subscriptions")).into_response(),
        401 => crate::to_login(),
        404 | 422 => {
            page(
                cx,
                None,
                Some(message(&out)),
                body,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}

/// Turn a plan off or on (a form post).
#[post("/admin/{shop}/subscription-plans/{id}/{action}", tag = "admin")]
pub(crate) async fn toggle_plan(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let active = match action.as_str() {
        "on" => true,
        "off" => false,
        _ => return Err(Error::not_found()),
    };
    let (status, out) = cx
        .call(
            Method::PATCH,
            &format!("/api/subscription-plans/{}", encode(&id)),
            Some(&json!({ "active": active })),
        )
        .await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/subscriptions")).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}

/// Pause, resume, skip the next order of, or cancel a subscription (a
/// form post).
#[post("/admin/{shop}/subscriptions/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    if !["pause", "resume", "skip", "cancel"].contains(&action.as_str()) {
        return Err(Error::not_found());
    }
    let (status, out) = cx
        .call(
            Method::POST,
            &format!("/api/subscriptions/{}/actions", encode(&id)),
            Some(&json!({ "action": action })),
        )
        .await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/subscriptions")).into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            page(
                cx,
                None,
                Some(message(&out)),
                Value::Null,
                StatusCode::CONFLICT,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}
