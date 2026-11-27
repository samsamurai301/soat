//! Marketplaces: how the shop sells on Amazon, eBay, Otto and others, and
//! the orders a connector brought in from them.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

const CHANNELS: &[(&str, &str)] = &[
    ("amazon", "Amazon"),
    ("ebay", "eBay"),
    ("otto", "Otto"),
    ("kaufland", "Kaufland"),
    ("zalando", "Zalando"),
    ("bol", "bol."),
    ("cdiscount", "Cdiscount"),
    ("allegro", "Allegro"),
    ("etsy", "Etsy"),
    ("other", "Another marketplace"),
];

/// A percentage like `15` or `12.5` (or `-10`) in basis points.
fn percent(text: &str) -> std::result::Result<i64, String> {
    let t = text.trim().trim_end_matches('%').trim().replace(',', ".");
    if t.is_empty() {
        return Ok(0);
    }
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, t),
    };
    let (whole, fraction) = t.split_once('.').unwrap_or((&t, ""));
    let ok = !whole.is_empty()
        && whole.len() <= 4
        && fraction.len() <= 2
        && whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit());
    if !ok {
        return Err(format!("“{text}” is not a percentage like 15 or 12.5."));
    }
    let bps: i64 = format!("{whole}{fraction:0<2}")
        .parse()
        .map_err(|_| format!("“{text}” is not a percentage."))?;
    Ok(if negative { -bps } else { bps })
}

/// The shop's marketplaces with their orders, and a form for a new one.
#[get("/admin/{shop}/marketplaces", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    Ok(page(cx, None, StatusCode::OK).await)
}

async fn page(mut cx: Ctx, error: Option<String>, status: StatusCode) -> Response {
    let (code, listed) = cx.call(Method::GET, "/api/marketplaces", None).await;
    match code {
        200 => {}
        404 => {
            cx.set("missing", json!(true));
            cx.set("page_title", json!("Marketplaces"));
            return cx.render("marketplaces", StatusCode::OK);
        }
        _ => return cx.failed(code, &listed),
    }
    let mut shown = Vec::new();
    for m in listed["items"].as_array().into_iter().flatten() {
        let mut m = m.clone();
        let currency = m["currency"].as_str().unwrap_or("EUR").to_owned();
        let bps = m["markup_bps"].as_i64().unwrap_or(0);
        m["markup"] = json!(format!(
            "{}{}.{:02} %",
            if bps < 0 { "-" } else { "" },
            bps.abs() / 100,
            bps.abs() % 100
        ));
        m["markup_amount"] = json!(money(m["markup_minor"].as_i64().unwrap_or(0), &currency));
        m["has_markup_amount"] = json!(m["markup_minor"].as_i64().unwrap_or(0) != 0);
        m["channel_name"] = json!(CHANNELS
            .iter()
            .find(|(k, _)| m["channel"] == *k)
            .map_or("Marketplace", |(_, n)| *n));
        let id = m["id"].as_str().unwrap_or_default().to_owned();
        let (_, orders) = cx
            .call(
                Method::GET,
                &format!("/api/marketplaces/{}/orders", encode(&id)),
                None,
            )
            .await;
        m["recent"] = Value::Array(
            orders["items"]
                .as_array()
                .into_iter()
                .flatten()
                .take(10)
                .map(|o| {
                    json!({
                        "external_id": o["external_id"],
                        "order_id": o["order"]["id"],
                        "number": o["order"]["number"],
                        "total": money(o["order"]["total_minor"].as_i64().unwrap_or(0), &currency),
                        "state": o["state"],
                        "pending": o["pending"],
                    })
                })
                .collect(),
        );
        shown.push(m);
    }
    cx.set("marketplaces", Value::Array(shown));
    cx.set(
        "channels",
        Value::Array(
            CHANNELS
                .iter()
                .map(|(k, n)| json!({ "key": k, "name": n }))
                .collect(),
        ),
    );
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Marketplaces"));
    cx.render("marketplaces", status)
}

/// Start selling on a marketplace (a form post).
#[post("/admin/{shop}/marketplaces", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let f = Fields::parse(&raw);
    let bps = match percent(&f.get("markup")) {
        Ok(v) => v,
        Err(e) => return Ok(page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await),
    };
    let buffer = f.get("buffer");
    let buffer: i64 = if buffer.trim().is_empty() {
        0
    } else {
        match buffer.trim().parse() {
            Ok(v) if v >= 0 => v,
            _ => {
                let e = "Units held back is a whole number, like 2.".to_owned();
                return Ok(page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            }
        }
    };
    let currency = f.get("currency").trim().to_uppercase();
    let b = json!({
        "name": f.get("name").trim(),
        "channel": f.get("channel"),
        "currency": Some(currency).filter(|c| !c.is_empty()),
        "markup_bps": bps,
        "stock_buffer": buffer,
        "all_products": f.get("only_listed") != "on",
    });
    let (status, out) = cx.call(Method::POST, "/api/marketplaces", Some(&b)).await;
    Ok(match status {
        201 => Redirect::to(&format!("/admin/{shop}/marketplaces")).into_response(),
        401 => crate::to_login(),
        409 | 422 => page(cx, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

/// Pause, resume or delete a marketplace (form posts).
#[post("/admin/{shop}/marketplaces/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "orders").await);
    let path = format!("/api/marketplaces/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "on" | "off" => {
            let b = json!({ "active": action == "on" });
            cx.call(Method::PATCH, &path, Some(&b)).await
        }
        "delete" => cx.call(Method::DELETE, &path, None).await,
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 | 204 => Redirect::to(&format!("/admin/{shop}/marketplaces")).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percentages() {
        assert_eq!(percent("15"), Ok(1500));
        assert_eq!(percent("12,5 %"), Ok(1250));
        assert_eq!(percent("-10"), Ok(-1000));
        assert_eq!(percent(""), Ok(0));
        assert!(percent("abc").is_err());
        assert!(percent("1.234").is_err());
    }
}
