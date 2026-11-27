//! Email campaigns: write one, send yourself a test, send it to the
//! customers who agreed to news, and see how it went.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// A campaign with what the page shows of its status.
fn shown(c: &Value) -> Value {
    let mut c = c.clone();
    let status = c["status"].as_str().unwrap_or_default().to_owned();
    c["draft"] = json!(status == "draft");
    c["sending"] = json!(status == "sending");
    c
}

/// The shop's campaigns and a form for a new one.
#[get("/admin/{shop}/campaigns", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    Ok(list_page(cx, None, Value::Null, StatusCode::OK).await)
}

async fn list_page(
    mut cx: Ctx,
    error: Option<String>,
    typed: Value,
    status: StatusCode,
) -> Response {
    let (code, page) = cx.call(Method::GET, "/api/campaigns", None).await;
    match code {
        200 => {}
        404 => cx.set("missing", json!(true)),
        _ => return cx.failed(code, &page),
    }
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(shown)
        .collect();
    cx.set("campaigns", Value::Array(items));
    cx.set("typed", typed);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Campaigns"));
    cx.render("campaigns", status)
}

fn body(f: &Fields) -> Value {
    let html = f.get("html_body");
    json!({
        "name": f.get("name"), "subject": f.get("subject"), "text_body": f.get("text_body"),
        "html_body": html, "audience_tag": f.get("audience_tag"),
    })
}

/// Write a new campaign (a form post).
#[post("/admin/{shop}/campaigns", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let typed = body(&f);
    let (status, out) = cx.call(Method::POST, "/api/campaigns", Some(&typed)).await;
    Ok(match status {
        201 => {
            let id = out["id"].as_str().unwrap_or_default();
            Redirect::to(&format!("/admin/{shop}/campaigns/{}", encode(id))).into_response()
        }
        401 => crate::to_login(),
        422 => {
            list_page(
                cx,
                Some(message(&out)),
                typed,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}

async fn show(mut cx: Ctx, id: &str, notice: Option<&str>, error: Option<String>) -> Response {
    let c = page_or!(cx.get(&format!("/api/campaigns/{}", encode(id))).await);
    cx.set("page_title", c["name"].clone());
    cx.set("campaign", shown(&c));
    if let Some(n) = notice {
        cx.set("notice", json!(n));
    }
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("campaign", status)
}

/// The campaign page's query.
#[model]
pub struct CampaignQuery {
    pub done: Option<String>,
}

/// A campaign: its wording while a draft, and how it went.
#[get("/admin/{shop}/campaigns/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<CampaignQuery>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let notice = match q.done.as_deref() {
        Some("saved") => Some("Saved."),
        Some("tested") => Some("The test is on its way."),
        Some("sending") => Some("Sending: it goes out in the background."),
        _ => None,
    };
    Ok(show(cx, &id, notice, None).await)
}

/// Save, test, send, cancel or delete a campaign (form posts).
#[post("/admin/{shop}/campaigns/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/campaigns/{}", encode(&id));
    let (status, out, done) = match action.as_str() {
        "save" => {
            let (s, o) = cx.call(Method::PATCH, &path, Some(&body(&f))).await;
            (s, o, "saved")
        }
        "test" => {
            let b = json!({ "email": f.get("email") });
            let (s, o) = cx
                .call(Method::POST, &format!("{path}/test"), Some(&b))
                .await;
            (s, o, "tested")
        }
        "send" => {
            let (s, o) = cx.call(Method::POST, &format!("{path}/send"), None).await;
            (s, o, "sending")
        }
        "cancel" => {
            let (s, o) = cx.call(Method::POST, &format!("{path}/cancel"), None).await;
            (s, o, "saved")
        }
        "delete" => {
            let (s, o) = cx.call(Method::DELETE, &path, None).await;
            if s == 204 {
                return Ok(Redirect::to(&format!("/admin/{shop}/campaigns")).into_response());
            }
            (s, o, "")
        }
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 | 204 => Redirect::to(&format!(
            "/admin/{shop}/campaigns/{}?done={done}",
            encode(&id)
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 | 503 => show(cx, &id, None, Some(message(&out))).await,
        _ => cx.failed(status, &out),
    })
}
