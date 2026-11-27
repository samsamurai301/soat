//! Product feeds for Google Merchant Center and Meta catalogs: their
//! URLs, and what the channels would reject.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// The shop's address for feed URLs: its first domain, else this server.
async fn shop_url(cx: &Ctx, headers: &HeaderMap) -> String {
    let (_, domains) = cx.call(Method::GET, "/api/domains", None).await;
    let first = domains["items"][0]["domain"].as_str().map(str::to_owned);
    match first {
        Some(d) => format!("https://{d}"),
        None => {
            let host = headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost:8080");
            format!("http://{host}")
        }
    }
}

/// The shop's feeds, each with its problems, and a form for a new one.
#[get("/admin/{shop}/feeds", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    Ok(page(cx, &headers, None, StatusCode::OK).await)
}

async fn page(
    mut cx: Ctx,
    headers: &HeaderMap,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let (code, listed) = cx.call(Method::GET, "/api/feeds", None).await;
    match code {
        200 => {}
        404 => {
            cx.set("missing", json!(true));
            cx.set("page_title", json!("Feeds"));
            return cx.render("feeds", StatusCode::OK);
        }
        _ => return cx.failed(code, &listed),
    }
    let base = shop_url(&cx, headers).await;
    let mut feeds = Vec::new();
    for f in listed["items"].as_array().into_iter().flatten() {
        let mut f = f.clone();
        f["url"] = json!(format!("{base}{}", f["path"].as_str().unwrap_or_default()));
        f["meta"] = json!(f["format"] == "meta");
        let id = f["id"].as_str().unwrap_or_default().to_owned();
        let (_, p) = cx
            .call(
                Method::GET,
                &format!("/api/feeds/{}/preview", encode(&id)),
                None,
            )
            .await;
        f["count"] = p["count"].clone();
        f["errors"] = p["errors"].clone();
        f["warnings"] = p["warnings"].clone();
        let problems: Vec<Value> = p["problems"]
            .as_array()
            .into_iter()
            .flatten()
            .take(30)
            .map(|x| {
                let mut x = x.clone();
                x["what"] = json!(match x["code"].as_str().unwrap_or_default() {
                    "missing_image" => "no picture",
                    "missing_description" => "no description",
                    "missing_title" => "no title",
                    "title_too_long" => "title over 150 characters",
                    "missing_price" => "no price",
                    "missing_link" => "no shop address (add a domain)",
                    "missing_gtin" => "no GTIN (custom field gtin)",
                    "invalid_gtin" => "the GTIN's check digit is wrong",
                    other => other,
                });
                x["error"] = json!(x["severity"] == "error");
                x
            })
            .collect();
        f["problems"] = Value::Array(problems);
        feeds.push(f);
    }
    cx.set("feeds", Value::Array(feeds));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Feeds"));
    cx.render("feeds", status)
}

/// Make a feed (a form post).
#[post("/admin/{shop}/feeds", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&raw);
    let currency = f.get("currency");
    let b = json!({
        "name": f.get("name").trim(),
        "format": f.get("format"),
        "currency": Some(currency.trim().to_uppercase()).filter(|c| !c.is_empty()),
        "brand": f.get("brand"),
        "collection_handle": f.get("collection_handle"),
        "include_out_of_stock": f.get("skip_out_of_stock") != "on",
    });
    let (status, out) = cx.call(Method::POST, "/api/feeds", Some(&b)).await;
    Ok(match status {
        201 => Redirect::to(&format!("/admin/{shop}/feeds")).into_response(),
        401 => crate::to_login(),
        422 => {
            page(
                cx,
                &headers,
                Some(message(&out)),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}

/// Turn a feed off or on, give it a new URL, or delete it (form posts).
#[post("/admin/{shop}/feeds/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let path = format!("/api/feeds/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "on" | "off" => {
            let b = json!({ "active": action == "on" });
            cx.call(Method::PATCH, &path, Some(&b)).await
        }
        "token" => cx.call(Method::POST, &format!("{path}/token"), None).await,
        "delete" => cx.call(Method::DELETE, &path, None).await,
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 | 204 => Redirect::to(&format!("/admin/{shop}/feeds")).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}
