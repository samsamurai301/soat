//! Product reviews: see what buyers wrote, publish or reject it, reply,
//! delete.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::encode;
use crate::{Ctx, Shared};

/// Which reviews the list shows.
#[model]
pub struct ReviewQuery {
    /// `pending` (the default), `published`, `rejected` or `all`.
    pub status: Option<String>,
}

/// The shop's product reviews, waiting ones first.
#[get("/admin/{shop}/reviews", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<ReviewQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let status = match q.status.as_deref() {
        Some(s @ ("published" | "rejected" | "all")) => s.to_owned(),
        _ => "pending".to_owned(),
    };
    let filter = if status == "all" {
        String::new()
    } else {
        format!("&status={status}")
    };
    let page = page!(cx.get(&format!("/api/reviews?limit=200{filter}")).await);
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let mut r = r.clone();
            let stars = usize::try_from(r["rating"].as_i64().unwrap_or(0)).unwrap_or(0);
            r["stars"] = json!("★".repeat(stars));
            r["date"] = json!(r["created_at"].as_str().unwrap_or_default().get(..10));
            r["is_pending"] = json!(r["status"] == "pending");
            r["is_published"] = json!(r["status"] == "published");
            r
        })
        .collect();
    cx.set("reviews", Value::Array(items));
    cx.set("status", json!({ status.as_str(): true, "name": status }));
    cx.set("page_title", json!("Reviews"));
    Ok(cx.render("reviews", StatusCode::OK))
}

/// Publish, reject, reply to or delete a review.
#[post("/admin/{shop}/reviews/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/reviews/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "publish" => {
            cx.call(
                Method::PATCH,
                &path,
                Some(&json!({ "status": "published" })),
            )
            .await
        }
        "reject" => {
            cx.call(Method::PATCH, &path, Some(&json!({ "status": "rejected" })))
                .await
        }
        "reply" => {
            cx.call(
                Method::PATCH,
                &path,
                Some(&json!({ "reply": f.get("reply") })),
            )
            .await
        }
        "delete" => cx.call(Method::DELETE, &path, None).await,
        _ => return Err(Error::not_found()),
    };
    let back = match f.get("back").as_str() {
        s @ ("published" | "rejected" | "all") => format!("?status={s}"),
        _ => String::new(),
    };
    Ok(match status {
        200 | 204 => Redirect::to(&format!("/admin/{shop}/reviews{back}")).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}
