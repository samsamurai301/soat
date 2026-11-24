//! Apps: the shop's installed apps, and the consent screen an app sends
//! its owner to (`/admin/{shop}/apps/authorize?client_id=…&redirect_uri=…&state=…`).

use serde_json::json;
use zapline::db::{Db, ListParams};
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::encode;
use crate::{Ctx, Shared};

/// The shop's installed apps.
#[get("/admin/{shop}/apps", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let apps = page!(cx.get("/api/app-installations").await);
    links(&mut cx, "app", None).await;
    cx.set("apps", apps);
    cx.set("page_title", json!("Apps"));
    Ok(cx.render("apps", StatusCode::OK))
}

/// The app directory: listed apps to find and install.
#[get("/admin/{shop}/apps/directory", tag = "admin")]
pub(crate) async fn directory(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    params: ListParams,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let q = params.get("q").unwrap_or_default().to_owned();
    let category = params.get("category").unwrap_or_default().to_owned();
    let path = format!(
        "/api/app-directory?q={}&category={}",
        encode(&q),
        encode(&category)
    );
    let found = page!(cx.get(&path).await);
    let categories: Vec<_> = found["categories"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| json!({ "name": c, "selected": c.as_str() == Some(category.as_str()) }))
        .collect();
    cx.set("apps", found["apps"].clone());
    cx.set("categories", json!(categories));
    cx.set("q", json!(q));
    cx.set("page_title", json!("App directory"));
    Ok(cx.render("app_directory", StatusCode::OK))
}

/// Uninstall an app (a form post): its key stops working at once.
#[post("/admin/{shop}/apps/{id}/uninstall", tag = "admin")]
pub(crate) async fn uninstall(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let path = format!("/api/app-installations/{}", encode(&id));
    let (status, out) = cx.call(Method::DELETE, &path, None).await;
    match status {
        204 => Ok(Redirect::to(&format!("/admin/{shop}/apps")).into_response()),
        401 => Ok(crate::to_login()),
        s => Ok(cx.failed(s, &out)),
    }
}

/// The consent screen: what the app asks to do in the shop.
#[get("/admin/{shop}/apps/authorize", tag = "admin")]
pub(crate) async fn authorize_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    params: ListParams,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let client_id = params.get("client_id").unwrap_or_default().to_owned();
    let consent = page!(
        cx.get(&format!("/api/app-consent/{}", encode(&client_id)))
            .await
    );
    let lacking = consent["permissions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| p["held"] == false);
    cx.set("consent", consent);
    cx.set("lacking", json!(lacking));
    cx.set(
        "redirect_uri",
        json!(params.get("redirect_uri").unwrap_or_default()),
    );
    cx.set("state", json!(params.get("state").unwrap_or_default()));
    cx.set("page_title", json!("Install an app"));
    Ok(cx.render("app_authorize", StatusCode::OK))
}

/// Install the app (a form post) and send the person back to it with a
/// one-time code.
#[post("/admin/{shop}/apps/authorize", tag = "admin")]
pub(crate) async fn authorize(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let f = Fields::parse(&body);
    let (status, out) = cx
        .call(
            Method::POST,
            "/api/app-installations",
            Some(&json!({
                "client_id": f.get("client_id"),
                "redirect_uri": f.get("redirect_uri"),
                "state": f.get("state"),
            })),
        )
        .await;
    match status {
        201 => {
            let to = out["redirect_to"].as_str().unwrap_or("/admin");
            Ok(Redirect::to(to).into_response())
        }
        401 => Ok(crate::to_login()),
        s => Ok(cx.failed(s, &out)),
    }
}

/// The links installed apps add to a page, as `app_links`; none when the
/// staff member may not see them or apps are off.
pub(crate) async fn links(cx: &mut Ctx, target: &str, id: Option<&str>) {
    let mut path = format!("/api/app-links?target={target}");
    if let Some(id) = id {
        path.push_str(&format!("&id={}", encode(id)));
    }
    let (status, links) = cx.call(Method::GET, &path, None).await;
    if status == 200 {
        cx.set("app_links", links);
    }
}
