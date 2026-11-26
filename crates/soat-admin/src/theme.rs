//! The theme: its settings, and its files to edit or reset.

use serde_json::{json, Map, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message};
use crate::{Ctx, Shared};

async fn overview(
    mut cx: Ctx,
    typed: Option<Value>,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let settings = match typed {
        Some(t) => t,
        None => page_or!(cx.get("/api/theme/settings").await),
    };
    let files = page_or!(cx.get("/api/theme/files").await);
    cx.set("page_title", json!("Theme"));
    cx.set("settings", settings);
    cx.set("files", files);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("theme", status)
}

/// The theme's settings and files.
#[get("/admin/{shop}/theme", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "theme").await);
    cx.set("saved", json!(q.contains_key("saved")));
    Ok(overview(cx, None, None, StatusCode::OK).await)
}

/// The settings form's fields.
#[model]
pub struct SettingsForm {
    pub accent_color: Option<String>,
    pub announcement: Option<String>,
    pub language: Option<String>,
}

/// Change the theme's settings (a form post); empty means the default.
#[post("/admin/{shop}/theme", tag = "admin")]
pub(crate) async fn save_settings(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<SettingsForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "theme").await);
    let opt = |v: &Option<String>| match v.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => json!(t),
        _ => Value::Null,
    };
    let body = json!({
        "accent_color": opt(&f.accent_color), "announcement": opt(&f.announcement),
        "language": opt(&f.language),
    });
    let (status, out) = cx
        .call(Method::PUT, "/api/theme/settings", Some(&body))
        .await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/theme?saved=1")).into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        422 => {
            overview(
                cx,
                Some(body),
                Some(message(&out)),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}

fn file_form(mut cx: Ctx, file: Value, error: Option<String>, status: StatusCode) -> Response {
    cx.set("page_title", json!(file["name"].clone()));
    cx.set("file", file);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("theme_file", status)
}

/// One theme file to edit.
#[get("/admin/{shop}/theme/files/{name}", tag = "admin")]
pub(crate) async fn edit_file(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, name)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "theme").await);
    let f = page!(cx.get(&format!("/api/theme/files/{}", encode(&name))).await);
    cx.set("saved", json!(q.contains_key("saved")));
    Ok(file_form(cx, f, None, StatusCode::OK))
}

/// The file form's fields.
#[model]
pub struct FileForm {
    pub source: String,
}

/// Save a theme file (a form post); a template that does not parse is
/// shown again with the reason.
#[post("/admin/{shop}/theme/files/{name}", tag = "admin")]
pub(crate) async fn save_file(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, name)): Path<(String, String)>,
    Form(f): Form<FileForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "theme").await);
    let path = format!("/api/theme/files/{}", encode(&name));
    // Browsers send CRLF line ends from a textarea.
    let source = f.source.replace("\r\n", "\n");
    let (status, out) = cx
        .call(Method::PUT, &path, Some(&json!({ "source": source })))
        .await;
    Ok(match status {
        200 => Redirect::to(&format!(
            "/admin/{shop}/theme/files/{}?saved=1",
            encode(&name)
        ))
        .into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        422 => {
            let file = json!({ "name": name, "source": source, "customized": true });
            file_form(
                cx,
                file,
                Some(message(&out)),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
        _ => cx.failed(status, &out),
    })
}

/// Back to the default file (a form post); a snippet is removed.
#[post("/admin/{shop}/theme/files/{name}/reset", tag = "admin")]
pub(crate) async fn reset_file(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, name)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "theme").await);
    let path = format!("/api/theme/files/{}", encode(&name));
    let (status, out) = cx.call(Method::DELETE, &path, None).await;
    Ok(match status {
        204 => Redirect::to(&format!("/admin/{shop}/theme")).into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        _ => cx.failed(status, &out),
    })
}
