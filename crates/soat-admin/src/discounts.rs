//! Discounts: codes buyers enter and automatic discounts, each with its
//! kind (a percentage, an amount, free shipping, buy X get Y), an optional
//! collection, minimum, dates and limits.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

const KINDS: &[(&str, &str)] = &[
    ("percentage", "Percentage off"),
    ("fixed_amount", "Amount off"),
    ("free_shipping", "Free shipping"),
    ("buy_x_get_y", "Buy X get Y"),
];

/// `12.5` as basis points (1250); at most two decimals, 0 to 100.
fn bps(text: &str) -> Option<i64> {
    let t = text.trim().trim_end_matches('%').trim().replace(',', ".");
    let (whole, frac) = t.split_once('.').unwrap_or((&t, ""));
    if frac.len() > 2 || whole.is_empty() && frac.is_empty() {
        return None;
    }
    let w: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let f: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<2}").parse().ok()?
    };
    let b = w.checked_mul(100)?.checked_add(f)?;
    (0..=10_000).contains(&b).then_some(b)
}

/// 1250 as `12.5`.
fn percent(bps: i64) -> String {
    if bps % 100 == 0 {
        (bps / 100).to_string()
    } else {
        format!("{}.{:02}", bps / 100, bps % 100)
            .trim_end_matches('0')
            .to_owned()
    }
}

/// What a discount does, in words.
fn summary(d: &Value, currency: &str) -> String {
    let v = d["value"].as_i64().unwrap_or(0);
    let cur = d["currency"].as_str().unwrap_or(currency);
    match d["kind"].as_str() {
        Some("percentage") => format!("{}% off", percent(v)),
        Some("fixed_amount") => format!("{} off", money(v, cur)),
        Some("free_shipping") => "Free shipping".into(),
        Some("buy_x_get_y") => {
            let got = if v == 10_000 {
                "free".to_owned()
            } else {
                format!("{}% off", percent(v))
            };
            format!(
                "Buy {} get {} {got}",
                d["buy_quantity"].as_i64().unwrap_or(0),
                d["get_quantity"].as_i64().unwrap_or(0)
            )
        }
        _ => String::new(),
    }
}

async fn currency(cx: &Ctx) -> String {
    let (_, shop) = cx.call(Method::GET, "/api/shop", None).await;
    shop["currency"].as_str().unwrap_or("EUR").to_owned()
}

/// The shop's discounts.
#[get("/admin/{shop}/discounts", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let page = page!(cx.get("/api/discounts?limit=200").await);
    let currency = currency(&cx).await;
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let mut d = d.clone();
            d["summary"] = json!(summary(&d, &currency));
            d
        })
        .collect();
    cx.set("discounts", Value::Array(items));
    cx.set("page_title", json!("Discounts"));
    Ok(cx.render("discounts", StatusCode::OK))
}

/// The discount's form: `d` as the API shows it, or as typed.
async fn show(mut cx: Ctx, d: Value, error: Option<String>, status: StatusCode) -> Response {
    let currency = currency(&cx).await;
    let (_, cols) = cx
        .call(Method::GET, "/api/collections?limit=200&sort=title", None)
        .await;
    let chosen = d["collection_id"].clone();
    let collections: Vec<Value> = cols["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| json!({ "id": c["id"], "title": c["title"], "selected": c["id"] == chosen }))
        .collect();
    let kind = d["kind"].as_str().unwrap_or("percentage").to_owned();
    let v = d["value"].as_i64().unwrap_or(0);
    let form = json!({
        "amount": match kind.as_str() {
            "fixed_amount" => d["currency"].as_str().and_then(|c| soat_core::Currency::parse(c).ok())
                .map(|c| c.decimal(v)).unwrap_or_default(),
            "free_shipping" => String::new(),
            _ if d["value"].is_null() => String::new(),
            _ => percent(v),
        },
        "min": d["min_subtotal_minor"].as_i64().and_then(|m| {
            soat_core::Currency::parse(d["currency"].as_str().unwrap_or(&currency)).ok().map(|c| c.decimal(m))
        }).unwrap_or_default(),
        "starts": d["starts_at"].as_str().map(|t| t.chars().take(10).collect::<String>()).unwrap_or_default(),
        "ends": d["ends_at"].as_str().map(|t| t.chars().take(10).collect::<String>()).unwrap_or_default(),
    });
    cx.set(
        "kinds",
        json!(KINDS
            .iter()
            .map(|(k, l)| json!({ "key": k, "label": l, "selected": *k == kind }))
            .collect::<Vec<_>>()),
    );
    cx.set("collections", Value::Array(collections));
    cx.set("form", form);
    cx.set("currency", json!(currency));
    cx.set("creating", json!(d["id"].is_null()));
    cx.set(
        "page_title",
        json!(d["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or("New discount")),
    );
    cx.set("discount", d);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("discount", status)
}

/// The form for a new discount.
#[get("/admin/{shop}/discounts/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let d = json!({ "kind": "percentage", "active": true, "automatic": false });
    Ok(show(cx, d, None, StatusCode::OK).await)
}

/// A discount's page.
#[get("/admin/{shop}/discounts/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let d = page!(cx.get(&format!("/api/discounts/{}", encode(&id))).await);
    Ok(show(cx, d, None, StatusCode::OK).await)
}

/// The form's discount as the API takes it.
fn body(f: &Fields, currency: &str) -> std::result::Result<Value, String> {
    let kind = f.get("kind");
    let automatic = f.get("mode") == "automatic";
    let typed = f.get("amount");
    let value = match kind.as_str() {
        "fixed_amount" => amount(&typed, currency)
            .ok_or_else(|| format!("Give the amount off in {currency}, like 5.00."))?,
        "free_shipping" => 0,
        _ => bps(&typed).ok_or("Give the percentage off, from 0.01 to 100.")?,
    };
    let number = |name: &str, what: &str| -> std::result::Result<Option<i64>, String> {
        let t = f.get(name);
        if t.is_empty() {
            return Ok(None);
        }
        t.parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| format!("{what} is a whole number above 0."))
    };
    let min = match f.get("min").as_str() {
        "" => None,
        t => Some(amount(t, currency).ok_or_else(|| format!("Give the minimum in {currency}."))?),
    };
    let date = |name: &str| -> Option<String> {
        let t = f.get(name);
        (!t.is_empty()).then(|| format!("{t}T00:00:00Z"))
    };
    let collection = f.get("collection_id");
    let title = f.get("title");
    let code = f.get("code");
    Ok(json!({
        "automatic": automatic,
        "code": if automatic || code.is_empty() { Value::Null } else { json!(code) },
        "title": if title.is_empty() { Value::Null } else { json!(title) },
        "kind": kind,
        "value": value,
        "buy_quantity": number("buy_quantity", "Buy")?,
        "get_quantity": number("get_quantity", "Get")?,
        "collection_id": if collection.is_empty() { Value::Null } else { json!(collection) },
        "currency": if kind == "fixed_amount" || min.is_some() { json!(currency) } else { Value::Null },
        "min_subtotal_minor": min,
        "starts_at": date("starts"),
        "ends_at": date("ends"),
        "usage_limit": number("usage_limit", "The limit")?,
        "once_per_customer": f.get("once_per_customer") == "on",
        "active": f.get("active") == "on",
    }))
}

/// The form as typed, to show again with a problem.
fn typed(f: &Fields, id: Option<&str>, currency: &str) -> Value {
    let kind = f.get("kind");
    let value = match kind.as_str() {
        "fixed_amount" => amount(&f.get("amount"), currency),
        _ => bps(&f.get("amount")),
    };
    let n = |name: &str| f.get(name).parse::<i64>().ok();
    let date = |name: &str| {
        let t = f.get(name);
        (!t.is_empty()).then_some(t)
    };
    json!({
        "id": id, "automatic": f.get("mode") == "automatic", "code": f.get("code"),
        "title": f.get("title"), "kind": kind, "value": value,
        "buy_quantity": n("buy_quantity"), "get_quantity": n("get_quantity"),
        "collection_id": f.get("collection_id"), "currency": currency,
        "min_subtotal_minor": amount(&f.get("min"), currency),
        "starts_at": date("starts"), "ends_at": date("ends"), "usage_limit": n("usage_limit"),
        "once_per_customer": f.get("once_per_customer") == "on", "active": f.get("active") == "on",
    })
}

/// Save the form: create when `id` is none.
async fn save(cx: Ctx, shop: &str, id: Option<&str>, raw: &[u8]) -> Response {
    let f = Fields::parse(raw);
    let currency = currency(&cx).await;
    let b = match body(&f, &currency) {
        Ok(b) => b,
        Err(e) => {
            let t = typed(&f, id, &currency);
            return show(cx, t, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await;
        }
    };
    let (status, out) = match id {
        None => cx.call(Method::POST, "/api/discounts", Some(&b)).await,
        Some(id) => {
            cx.call(
                Method::PUT,
                &format!("/api/discounts/{}", encode(id)),
                Some(&b),
            )
            .await
        }
    };
    match status {
        200 | 201 => Redirect::to(&format!(
            "/admin/{shop}/discounts/{}",
            encode(out["id"].as_str().unwrap_or_default())
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            let t = typed(&f, id, &currency);
            show(cx, t, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await
        }
        _ => cx.failed(status, &out),
    }
}

/// Create a discount (a form post).
#[post("/admin/{shop}/discounts", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    Ok(save(cx, &shop, None, &raw).await)
}

/// Save a discount (a form post).
#[post("/admin/{shop}/discounts/{id}", tag = "admin")]
pub(crate) async fn update(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    Ok(save(cx, &shop, Some(&id), &raw).await)
}

/// Delete a discount (a form post).
#[post("/admin/{shop}/discounts/{id}/delete", tag = "admin")]
pub(crate) async fn delete(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let (status, out) = cx
        .call(
            Method::DELETE,
            &format!("/api/discounts/{}", encode(&id)),
            None,
        )
        .await;
    Ok(match status {
        204 => Redirect::to(&format!("/admin/{shop}/discounts")).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages() {
        assert_eq!(bps("10"), Some(1000));
        assert_eq!(bps("12.5"), Some(1250));
        assert_eq!(bps("12,05 %"), Some(1205));
        assert_eq!(bps("100"), Some(10_000));
        assert_eq!(bps("101"), None);
        assert_eq!(bps("1.234"), None);
        assert_eq!(bps(""), None);
        assert_eq!(percent(1250), "12.5");
        assert_eq!(percent(1000), "10");
    }
}
