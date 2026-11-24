//! The blog: posts in Markdown, drafts, scheduled and published.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// The shop's posts, newest first.
#[get("/admin/{shop}/blog", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let page = page!(cx.get("/api/blog/posts?limit=200").await);
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "id": p["id"], "title": p["title"], "handle": p["handle"],
                "published": p["published"], "author": p["author"],
                "date": p["published_at"].as_str().and_then(|d| d.get(..10)),
            })
        })
        .collect();
    cx.set("tab", json!({ "blog": true }));
    cx.set("posts", Value::Array(items));
    cx.set("page_title", json!("Blog"));
    Ok(cx.render("blog", StatusCode::OK))
}

/// The post form, for `post` as the API shows it or as typed.
fn show(mut cx: Ctx, post: Value, error: Option<String>, status: StatusCode) -> Response {
    cx.set("tab", json!({ "blog": true }));
    cx.set(
        "page_title",
        json!(post["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or("New post")),
    );
    cx.set("creating", json!(post["id"].is_null()));
    cx.set(
        "tags",
        json!(post["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")),
    );
    cx.set(
        "date",
        json!(post["published_at"]
            .as_str()
            .map(|d| d.chars().take(16).collect::<String>())
            .unwrap_or_default()),
    );
    cx.set("post", post);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("post", status)
}

/// The form for a new post.
#[get("/admin/{shop}/blog/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let post = json!({ "title": "", "body": "", "published": false });
    Ok(show(cx, post, None, StatusCode::OK))
}

/// A post's page.
#[get("/admin/{shop}/blog/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let post = page!(cx.get(&format!("/api/blog/posts/{}", encode(&id))).await);
    Ok(show(cx, post, None, StatusCode::OK))
}

/// The form's post as the API takes it.
fn body(f: &Fields) -> Value {
    let opt = |name: &str| {
        let v = f.get(name);
        if v.is_empty() {
            Value::Null
        } else {
            json!(v)
        }
    };
    let date = f.get("date");
    json!({
        "title": f.get("title"), "handle": opt("handle"), "excerpt": f.get("excerpt"),
        "body": f.get("body"), "image_url": opt("image_url"), "author": f.get("author"),
        "tags": f.get("tags").split(',').map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>(),
        "seo_title": opt("seo_title"), "seo_description": opt("seo_description"),
        "published": f.get("published") == "on",
        "published_at": if date.is_empty() { Value::Null } else { json!(format!("{date}:00Z")) },
    })
}

async fn save(cx: Ctx, shop: &str, id: Option<&str>, raw: &[u8]) -> Response {
    let f = Fields::parse(raw);
    let b = body(&f);
    let (status, out) = match id {
        None => cx.call(Method::POST, "/api/blog/posts", Some(&b)).await,
        Some(id) => {
            cx.call(
                Method::PUT,
                &format!("/api/blog/posts/{}", encode(id)),
                Some(&b),
            )
            .await
        }
    };
    match status {
        200 | 201 => Redirect::to(&format!(
            "/admin/{shop}/blog/{}",
            encode(out["id"].as_str().unwrap_or_default())
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            let mut typed = b;
            typed["id"] = json!(id);
            let date = f.get("date");
            typed["published_at"] = if date.is_empty() {
                Value::Null
            } else {
                json!(date)
            };
            show(
                cx,
                typed,
                Some(message(&out)),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
        _ => cx.failed(status, &out),
    }
}

/// Create a post (a form post).
#[post("/admin/{shop}/blog", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    Ok(save(cx, &shop, None, &raw).await)
}

/// Save a post (a form post).
#[post("/admin/{shop}/blog/{id}", tag = "admin")]
pub(crate) async fn update(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    Ok(save(cx, &shop, Some(&id), &raw).await)
}
