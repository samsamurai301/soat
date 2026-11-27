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
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Privacy requests"));
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
