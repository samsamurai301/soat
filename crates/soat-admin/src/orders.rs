//! Orders: a list with quick views, and an order's page to take payment,
//! ship, refund or cancel.

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

const PER_PAGE: i64 = 50;

/// The list's filters.
#[model]
pub struct OrderQuery {
    /// Searches emails.
    pub q: Option<String>,
    /// `open`, `unpaid`, `unfulfilled` or `canceled`; all when absent.
    pub view: Option<String>,
    pub offset: Option<i64>,
}

fn label(s: &str) -> String {
    let s = s.replace('_', " ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Orders, newest first, 50 a page.
#[get("/admin/{shop}/orders", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(f): Query<OrderQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let q = f.q.unwrap_or_default().trim().to_owned();
    let view = f.view.unwrap_or_default();
    let offset = f.offset.unwrap_or(0).max(0);
    let mut query = vec![
        ("limit", PER_PAGE.to_string()),
        ("offset", offset.to_string()),
    ];
    match view.as_str() {
        "open" => query.push(("status", "open".into())),
        "unpaid" => {
            query.push(("status", "open".into()));
            query.push(("financial_status", "pending".into()));
        }
        "unfulfilled" => {
            query.push(("status", "open".into()));
            query.push(("fulfillment_status", "unfulfilled".into()));
        }
        "canceled" => query.push(("status", "canceled".into())),
        _ => {}
    }
    if !q.is_empty() {
        query.push(("q", q.clone()));
    }
    let qs = serde_urlencoded::to_string(&query).unwrap_or_default();
    let page = page!(cx.get(&format!("/api/orders?{qs}")).await);
    let rows: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            json!({
                "id": o["id"],
                "number": o["number"],
                "email": o["email"],
                "placed": o["placed_at"].as_str().and_then(|p| p.get(..10)),
                "canceled": o["status"] == "canceled",
                "paid": o["financial_status"] == "paid",
                "shipped": o["fulfillment_status"] == "fulfilled",
                "financial": label(o["financial_status"].as_str().unwrap_or_default()),
                "fulfillment": label(o["fulfillment_status"].as_str().unwrap_or_default()),
                "total": money(
                    o["total_minor"].as_i64().unwrap_or(0),
                    o["currency"].as_str().unwrap_or_default(),
                ),
            })
        })
        .collect();
    let total = page["total"].as_i64().unwrap_or(0);
    let link = |at: i64| {
        let mut q2 = vec![("offset", at.to_string())];
        if !q.is_empty() {
            q2.push(("q", q.clone()));
        }
        if !view.is_empty() {
            q2.push(("view", view.clone()));
        }
        format!(
            "/admin/{shop}/orders?{}",
            serde_urlencoded::to_string(&q2).unwrap_or_default()
        )
    };
    if offset > 0 {
        cx.set("prev_url", json!(link((offset - PER_PAGE).max(0))));
    }
    if offset + PER_PAGE < total {
        cx.set("next_url", json!(link(offset + PER_PAGE)));
    }
    let current = if view.is_empty() {
        "all"
    } else {
        view.as_str()
    };
    cx.set("page_title", json!("Orders"));
    cx.set("orders", Value::Array(rows));
    cx.set("total", json!(total));
    cx.set("q", json!(q));
    cx.set("view", json!({ current: true }));
    Ok(cx.render("orders", StatusCode::OK))
}

fn address(a: &Value) -> Value {
    if a.is_null() {
        return Value::Null;
    }
    let s = |k: &str| a[k].as_str().unwrap_or_default().trim().to_owned();
    let name = format!("{} {}", s("first_name"), s("last_name"));
    let lines: Vec<String> = [
        name.trim().to_owned(),
        s("company"),
        s("line1"),
        s("line2"),
        format!("{} {}", s("postal_code"), s("city"))
            .trim()
            .to_owned(),
        s("region"),
        soat_core::address::country_name(&s("country"))
            .unwrap_or_default()
            .to_owned(),
        s("phone"),
    ]
    .into_iter()
    .filter(|l| !l.is_empty())
    .collect();
    json!(lines)
}

/// An order's page; `error` shows what an action could not do.
async fn detail(mut cx: Ctx, id: &str, error: Option<String>, status: StatusCode) -> Response {
    let o = page_or!(cx.get(&format!("/api/orders/{}", encode(id))).await);
    let pays = page_or!(
        cx.get(&format!("/api/payments?reference={}&limit=50", encode(id)))
            .await
    );
    let cur = o["currency"].as_str().unwrap_or_default().to_owned();
    let m = |v: &Value| money(v.as_i64().unwrap_or(0), &cur);
    let some = |v: &Value| (v.as_i64().unwrap_or(0) != 0).then(|| m(v));
    let lines: Vec<Value> = o["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            json!({
                "title": l["title"], "sku": l["sku"], "quantity": l["quantity"],
                "shipped": l["fulfilled_quantity"],
                "price": m(&l["unit_price_minor"]), "total": m(&l["total_minor"]),
            })
        })
        .collect();
    let mut paid = 0;
    let payments: Vec<Value> = pays["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let st = p["status"].as_str().unwrap_or_default();
            if matches!(st, "paid" | "partially_refunded" | "refunded") {
                paid += p["amount_minor"].as_i64().unwrap_or(0)
                    - p["refunded_minor"].as_i64().unwrap_or(0);
            }
            json!({
                "id": p["id"], "provider": label(p["provider"].as_str().unwrap_or_default()),
                "status": label(st), "amount": m(&p["amount_minor"]),
                "refunded": some(&p["refunded_minor"]),
                "can_mark_paid": st == "pending" && p["provider"] == "manual",
                "can_capture": st == "authorized",
            })
        })
        .collect();
    let shipments: Vec<Value> = o["fulfillments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            let st = f["status"].as_str().unwrap_or_default();
            let units: i64 = f["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|l| l["quantity"].as_i64().unwrap_or(0))
                .sum();
            json!({
                "id": f["id"], "status": label(st), "units": units,
                "carrier": f["carrier"], "tracking_number": f["tracking_number"],
                "tracking_url": f["tracking_url"].as_str().filter(|u| u.starts_with("https://")),
                "date": f["created_at"].as_str().and_then(|d| d.get(..10)),
                "open": st == "shipped",
            })
        })
        .collect();
    let open = o["status"] == "open";
    let fulfillment = o["fulfillment_status"].as_str().unwrap_or_default();
    cx.set("page_title", json!(format!("Order #{}", o["number"])));
    cx.set(
        "order",
        json!({
            "id": o["id"], "number": o["number"], "email": o["email"],
            "placed": o["placed_at"].as_str().and_then(|p| p.get(..10)),
            "status": label(o["status"].as_str().unwrap_or_default()),
            "financial": label(o["financial_status"].as_str().unwrap_or_default()),
            "fulfillment": label(fulfillment),
            "canceled": !open, "cancel_reason": o["cancel_reason"], "note": o["note"],
            "subtotal": m(&o["subtotal_minor"]), "shipping": m(&o["shipping_minor"]),
            "shipping_name": o["shipping_name"], "discount": some(&o["discount_minor"]),
            "discount_code": o["discount_code"], "tax": m(&o["tax_minor"]),
            "total": m(&o["total_minor"]), "currency": cur,
            "shipping_address": address(&o["shipping_address"]),
            "billing_address": address(&o["billing_address"]),
            "vat_number": o["vat_number"], "reverse_charge": o["reverse_charge"],
            "can_ship": open && fulfillment != "fulfilled",
            "can_cancel": open && fulfillment == "unfulfilled",
            "can_refund": paid > 0,
            "refundable": Currency::parse(&cur).map(|c| c.decimal(paid)).unwrap_or_default(),
        }),
    );
    cx.set("lines", Value::Array(lines));
    cx.set("payments", Value::Array(payments));
    cx.set("shipments", Value::Array(shipments));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("order", status)
}

/// One order.
#[get("/admin/{shop}/orders/{id}", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<serde_json::Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    if let Some(done) = q.get("done").and_then(Value::as_str) {
        let text = match done {
            "shipped" => "Shipped.",
            "delivered" => "Marked delivered.",
            "unshipped" => "Shipment canceled; the units are back in stock.",
            "paid" => "Payment marked paid.",
            "captured" => "Payment captured.",
            "refunded" => "Refunded.",
            "canceled" => "Order canceled.",
            _ => "Done.",
        };
        cx.set("done", json!(text));
    }
    Ok(detail(cx, &id, None, StatusCode::OK).await)
}

/// Runs an action on the API; back to the order, saying what happened or
/// what went wrong.
async fn act(cx: Ctx, id: &str, method: Method, path: &str, body: Value, done: &str) -> Response {
    let (status, out) = cx.call(method, path, Some(&body)).await;
    match status {
        200..=299 => Redirect::to(&format!(
            "/admin/{}/orders/{}?done={done}",
            cx.shop,
            encode(id)
        ))
        .into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        403 | 404 => cx.failed(status, &out),
        _ => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            detail(cx, id, Some(message(&out)), code).await
        }
    }
}

/// The ship form's fields.
#[model]
pub struct ShipForm {
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub tracking_url: Option<String>,
}

fn opt(v: &Option<String>) -> Value {
    match v.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => json!(t),
        _ => Value::Null,
    }
}

/// Ship everything not shipped yet (a form post), with tracking.
#[post("/admin/{shop}/orders/{id}/ship", tag = "admin")]
pub(crate) async fn ship(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<ShipForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let body = json!({
        "carrier": opt(&f.carrier), "tracking_number": opt(&f.tracking_number),
        "tracking_url": opt(&f.tracking_url),
    });
    let path = format!("/api/orders/{}/fulfillments", encode(&id));
    Ok(act(cx, &id, Method::POST, &path, body, "shipped").await)
}

/// The cancel form's fields.
#[model]
pub struct CancelForm {
    pub reason: Option<String>,
}

/// Cancel the order (a form post); stock held for it is released.
#[post("/admin/{shop}/orders/{id}/cancel", tag = "admin")]
pub(crate) async fn cancel(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<CancelForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let path = format!("/api/orders/{}/cancel", encode(&id));
    let body = json!({ "reason": opt(&f.reason) });
    Ok(act(cx, &id, Method::POST, &path, body, "canceled").await)
}

/// The refund form's fields.
#[model]
pub struct RefundForm {
    /// e.g. `19.99`, in the order's currency.
    pub amount: String,
    pub reason: Option<String>,
}

/// Refund an amount over the order's payments (a form post).
#[post("/admin/{shop}/orders/{id}/refund", tag = "admin")]
pub(crate) async fn refund(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<RefundForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let o = page!(cx.get(&format!("/api/orders/{}", encode(&id))).await);
    let cur = o["currency"].as_str().unwrap_or_default().to_owned();
    let amount = Currency::parse(&cur)
        .ok()
        .and_then(|c| c.parse_decimal(f.amount.trim()))
        .filter(|a| *a > 0);
    let Some(amount) = amount else {
        let e = format!("The amount is a sum in {cur}, like 10.00.");
        return Ok(detail(cx, &id, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
    };
    let path = format!("/api/orders/{}/refunds", encode(&id));
    let body = json!({ "amount_minor": amount, "reason": opt(&f.reason) });
    Ok(act(cx, &id, Method::POST, &path, body, "refunded").await)
}

/// Act on one of the order's payments or shipments (a form post):
/// `mark-paid` or `capture` a payment, `delivered` or `cancel` a shipment.
#[post("/admin/{shop}/orders/{id}/{kind}/{item}/{action}", tag = "admin")]
pub(crate) async fn item_action(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, kind, item, action)): Path<(String, String, String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let (path, done) = match (kind.as_str(), action.as_str()) {
        ("payments", "mark-paid") => ("payments", "paid"),
        ("payments", "capture") => ("payments", "captured"),
        ("shipments", "delivered") => ("fulfillments", "delivered"),
        ("shipments", "cancel") => ("fulfillments", "unshipped"),
        _ => return Ok(cx.failed(404, &Value::Null)),
    };
    // Only this order's payments and shipments.
    let o = page!(cx.get(&format!("/api/orders/{}", encode(&id))).await);
    let belongs = if path == "payments" {
        let pays = page!(
            cx.get(&format!("/api/payments?reference={}&limit=50", encode(&id)))
                .await
        );
        pays["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|p| p["id"] == item.as_str()))
    } else {
        o["fulfillments"]
            .as_array()
            .is_some_and(|a| a.iter().any(|f| f["id"] == item.as_str()))
    };
    if !belongs {
        return Ok(cx.failed(404, &Value::Null));
    }
    let api = format!("/api/{path}/{}/{}", encode(&item), action);
    Ok(act(cx, &id, Method::POST, &api, json!({}), done).await)
}
