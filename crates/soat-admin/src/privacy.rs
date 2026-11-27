//! Privacy: a customer's data downloaded or erased (GDPR), and customers'
//! requests to be erased.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// A customer's personal data as a JSON file, to hand to them.
#[get("/admin/{shop}/customers/{id}/personal-data", tag = "admin")]
pub(crate) async fn download(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let (status, out) = cx
        .call(
            Method::GET,
            &format!("/api/customers/{}/personal-data", encode(&id)),
            None,
        )
        .await;
    if status != 200 {
        return Ok(cx.failed(status, &out));
    }
    let body = serde_json::to_string_pretty(&out).unwrap_or_default();
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    let name = format!("personal-data-{}.json", encode(&id));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

/// Erase a customer, after their email is typed again (a form post); shows
/// what happened to each kind of data.
#[post("/admin/{shop}/customers/{id}/erase", tag = "admin")]
pub(crate) async fn erase(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let b = json!({ "email": f.get("email") });
    let (status, out) = cx
        .call(
            Method::POST,
            &format!("/api/customers/{}/erase", encode(&id)),
            Some(&b),
        )
        .await;
    match status {
        200 => {
            let tables: Vec<Value> = out["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["rows"].as_i64().unwrap_or(0) > 0)
                .cloned()
                .collect();
            cx.set(
                "report",
                json!({ "tables": tables, "events": out["events_redacted"] }),
            );
            Ok(requests_page(cx, None, StatusCode::OK).await)
        }
        401 => Ok(crate::to_login()),
        422 => {
            let e = format!("Not erased: {}", message(&out));
            Ok(requests_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await)
        }
        _ => Ok(cx.failed(status, &out)),
    }
}

/// Customers' requests to be erased.
#[get("/admin/{shop}/privacy", tag = "admin")]
pub(crate) async fn requests(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    Ok(requests_page(cx, None, StatusCode::OK).await)
}

async fn requests_page(mut cx: Ctx, error: Option<String>, status: StatusCode) -> Response {
    let (code, listed) = cx.call(Method::GET, "/api/privacy-requests", None).await;
    match code {
        200 => {
            let items: Vec<Value> = listed["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let mut r = r.clone();
                    r["open"] = json!(r["status"] == "open");
                    r
                })
                .collect();
            cx.set("requests", Value::Array(items));
        }
        404 => cx.set("missing", json!(true)),
        _ => return cx.failed(code, &listed),
    }
    let (_, banner) = cx.call(Method::GET, "/api/consent", None).await;
    let (_, stats) = cx.call(Method::GET, "/api/consent/stats", None).await;
    cx.set("banner", banner);
    cx.set("stats", stats);
    let (got, retention) = cx.call(Method::GET, "/api/retention", None).await;
    if got == 200 {
        cx.set("retention", retention);
    }
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Privacy"));
    cx.render("privacy", status)
}

/// Decline a request, with the reason (a form post).
#[post("/admin/{shop}/privacy/{id}/decline", tag = "admin")]
pub(crate) async fn decline(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let b = json!({ "note": f.get("note").trim() });
    let (status, out) = cx
        .call(
            Method::POST,
            &format!("/api/privacy-requests/{}/decline", encode(&id)),
            Some(&b),
        )
        .await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/privacy")).into_response(),
        401 => crate::to_login(),
        409 | 422 => requests_page(cx, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

/// Set up the cookie banner (a form post).
#[post("/admin/{shop}/privacy/consent", tag = "admin")]
pub(crate) async fn consent(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let b = json!({
        "enabled": f.get("enabled") == "on",
        "privacy_url": f.get("privacy_url"),
        "ga4_id": f.get("ga4_id"),
        "meta_pixel_id": f.get("meta_pixel_id"),
        "ask_again": f.get("ask_again") == "on",
    });
    let (status, out) = cx.call(Method::PUT, "/api/consent", Some(&b)).await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/privacy")).into_response(),
        401 => crate::to_login(),
        422 => requests_page(cx, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

/// Set how long each kind of data is kept (a form post: `days_{rule}`,
/// blank keeps it).
#[post("/admin/{shop}/privacy/retention", tag = "admin")]
pub(crate) async fn retention_settings(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let (got, current) = cx.call(Method::GET, "/api/retention", None).await;
    if got != 200 {
        return Ok(cx.failed(got, &current));
    }
    let mut days = serde_json::Map::new();
    for r in current["rules"].as_array().into_iter().flatten() {
        let Some(key) = r["key"].as_str() else {
            continue;
        };
        let raw = f.get(&format!("days_{key}"));
        let raw = raw.trim();
        let value = if raw.is_empty() {
            Value::Null
        } else {
            match raw.parse::<i64>() {
                Ok(d) => json!(d),
                Err(_) => {
                    let m = format!(
                        "{} needs a number of days, or nothing to keep it.",
                        r["label"].as_str().unwrap_or(key)
                    );
                    return Ok(requests_page(cx, Some(m), StatusCode::UNPROCESSABLE_ENTITY).await);
                }
            }
        };
        days.insert(key.to_owned(), value);
    }
    let (status, out) = cx
        .call(
            Method::PUT,
            "/api/retention",
            Some(&json!({ "days": days })),
        )
        .await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/privacy")).into_response(),
        401 => crate::to_login(),
        422 => requests_page(cx, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

/// Remove now what is past its time (a form post).
#[post("/admin/{shop}/privacy/retention/sweep", tag = "admin")]
pub(crate) async fn retention_sweep(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let (status, out) = cx.call(Method::POST, "/api/retention/sweep", None).await;
    Ok(match status {
        200 => {
            let n = out["total"].as_i64().unwrap_or(0);
            cx.set(
                "swept",
                json!(if n == 1 {
                    "Swept: 1 record removed.".to_owned()
                } else {
                    format!("Swept: {n} records removed.")
                }),
            );
            requests_page(cx, None, StatusCode::OK).await
        }
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}
