//! Content: pages built from blocks, menus, and URL redirects.

use serde_json::{json, Map, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message};
use crate::{Ctx, Shared};

fn text(v: &Option<String>) -> String {
    v.as_deref().map(str::trim).unwrap_or_default().to_owned()
}

/// Puts `tab` into the content section's tabs.
fn tab(cx: &mut Ctx, tab: &str) {
    cx.set("tab", json!({ tab: true }));
}

/// Pages, by title.
#[get("/admin/{shop}/pages", tag = "admin")]
pub(crate) async fn pages(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let list = page!(cx.get("/api/pages?limit=200&sort=title").await);
    let rows: Vec<Value> = list["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "id": p["id"], "title": p["title"], "handle": p["handle"],
                "published": p["published"],
                "updated": p["updated_at"].as_str().and_then(|d| d.get(..10)),
            })
        })
        .collect();
    tab(&mut cx, "pages");
    cx.set("page_title", json!("Pages"));
    cx.set("pages", Value::Array(rows));
    Ok(cx.render("pages", StatusCode::OK))
}

/// The page form's fields, as typed.
#[model]
#[derive(Default)]
pub struct PageForm {
    pub title: Option<String>,
    pub handle: Option<String>,
    /// Present when the box is ticked.
    pub published: Option<String>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    /// The blocks as JSON.
    pub blocks: Option<String>,
}

impl PageForm {
    fn body(&self) -> std::result::Result<Value, String> {
        let raw = text(&self.blocks);
        let blocks: Value = if raw.is_empty() {
            json!([])
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| format!("The blocks are not valid JSON: {e}."))?
        };
        if !blocks.is_array() {
            return Err("The blocks are a JSON list: [ … ].".into());
        }
        let opt = |v: &Option<String>| {
            let t = text(v);
            (!t.is_empty()).then_some(t)
        };
        Ok(json!({
            "title": text(&self.title), "handle": opt(&self.handle), "blocks": blocks,
            "seo_title": opt(&self.seo_title), "seo_description": opt(&self.seo_description),
            "published": self.published.is_some(),
        }))
    }

    fn typed(&self) -> Value {
        json!({
            "title": text(&self.title), "handle": text(&self.handle),
            "published": self.published.is_some(), "seo_title": text(&self.seo_title),
            "seo_description": text(&self.seo_description),
            "blocks": self.blocks.clone().unwrap_or_default(),
        })
    }

    fn of(p: &Value) -> Value {
        let mut v = p.clone();
        v["blocks"] = json!(serde_json::to_string_pretty(&p["blocks"]).unwrap_or_default());
        v
    }
}

const BLOCK_HELP: &str = r#"[
  { "type": "heading", "text": "About us", "level": 1 },
  { "type": "text", "body": "We make *shoes*." },
  { "type": "image", "url": "https://…", "alt": "Our workshop" },
  { "type": "button", "label": "Shop now", "url": "/collections/all" },
  { "type": "products", "title": "Bestsellers", "collection_id": "col_…", "limit": 8 },
  { "type": "columns", "columns": [[ …blocks… ], [ …blocks… ]] }
]"#;

fn page_form(mut cx: Ctx, page: Value, error: Option<String>, status: StatusCode) -> Response {
    let creating = page["id"].is_null();
    let title = if creating {
        "New page".to_owned()
    } else {
        page["title"].as_str().unwrap_or("Page").to_owned()
    };
    tab(&mut cx, "pages");
    cx.set("page_title", json!(title));
    cx.set("page", page);
    cx.set("creating", json!(creating));
    cx.set("block_help", json!(BLOCK_HELP));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("page", status)
}

/// The form for a new page.
#[get("/admin/{shop}/pages/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let mut blank = PageForm::default().typed();
    blank["blocks"] = json!("[\n  { \"type\": \"text\", \"body\": \"\" }\n]");
    Ok(page_form(cx, blank, None, StatusCode::OK))
}

/// Answers a page create or replace: to the page, or the form again.
async fn saved(cx: Ctx, f: &PageForm, id: Option<&str>, status: u16, out: &Value) -> Response {
    match status {
        200 | 201 => {
            let id = id.or(out["id"].as_str()).unwrap_or_default();
            Redirect::to(&format!("/admin/{}/pages/{}?saved=1", cx.shop, encode(id)))
                .into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => {
            let mut typed = f.typed();
            if let Some(id) = id {
                typed["id"] = json!(id);
            }
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            page_form(cx, typed, Some(message(out)), code)
        }
        _ => cx.failed(status, out),
    }
}

/// Create a page (a form post).
#[post("/admin/{shop}/pages", tag = "admin")]
pub(crate) async fn create_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<PageForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let body = match f.body() {
        Ok(b) => b,
        Err(e) => {
            let typed = f.typed();
            return Ok(page_form(
                cx,
                typed,
                Some(e),
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
    };
    let (status, out) = cx.call(Method::POST, "/api/pages", Some(&body)).await;
    Ok(saved(cx, &f, None, status, &out).await)
}

/// A page's form.
#[get("/admin/{shop}/pages/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let p = page!(cx.get(&format!("/api/pages/{}", encode(&id))).await);
    cx.set("saved", json!(q.contains_key("saved")));
    Ok(page_form(cx, PageForm::of(&p), None, StatusCode::OK))
}

/// Replace a page (a form post).
#[post("/admin/{shop}/pages/{id}", tag = "admin")]
pub(crate) async fn update_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<PageForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let body = match f.body() {
        Ok(b) => b,
        Err(e) => {
            let mut typed = f.typed();
            typed["id"] = json!(id);
            return Ok(page_form(
                cx,
                typed,
                Some(e),
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
    };
    let path = format!("/api/pages/{}", encode(&id));
    let (status, out) = cx.call(Method::PUT, &path, Some(&body)).await;
    Ok(saved(cx, &f, Some(&id), status, &out).await)
}

/// Delete a page, menu or redirect (a form post).
#[post("/admin/{shop}/{kind}/{id}/delete", tag = "admin")]
pub(crate) async fn delete(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, kind, id)): Path<(String, String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    if !matches!(kind.as_str(), "pages" | "menus" | "redirects") {
        return Ok(cx.failed(404, &Value::Null));
    }
    let (status, out) = cx
        .call(
            Method::DELETE,
            &format!("/api/{kind}/{}", encode(&id)),
            None,
        )
        .await;
    Ok(match status {
        204 => Redirect::to(&format!("/admin/{shop}/{kind}")).into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        _ => cx.failed(status, &out),
    })
}

/// Menu items as lines a person edits: `Title | /url`, one dash more per
/// level (`- Team | /pages/team`).
pub(crate) fn menu_text(items: &Value) -> String {
    fn walk(items: &Value, depth: usize, out: &mut String) {
        for i in items.as_array().into_iter().flatten() {
            if depth > 0 {
                out.push_str(&"-".repeat(depth));
                out.push(' ');
            }
            out.push_str(i["title"].as_str().unwrap_or_default());
            out.push_str(" | ");
            out.push_str(i["url"].as_str().unwrap_or_default());
            out.push('\n');
            walk(&i["items"], depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(items, 0, &mut out);
    out
}

/// The lines of [`menu_text`] as menu items; a line without `|`, or one
/// nested two levels below its parent, is an error naming it.
pub(crate) fn menu_items(text: &str) -> std::result::Result<Value, String> {
    // (depth, item) in order; then folded into a tree.
    let mut flat: Vec<(usize, String, String)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let depth = line.chars().take_while(|c| *c == '-').count();
        let rest = line[depth..].trim();
        let Some((title, url)) = rest.split_once('|') else {
            return Err(format!("Line {} needs a title, then | and a link.", n + 1));
        };
        let prev = flat.last().map_or(0, |(d, ..)| *d);
        if depth > prev + 1 || (flat.is_empty() && depth > 0) {
            return Err(format!("Line {} is nested too deep for its place.", n + 1));
        }
        flat.push((depth, title.trim().to_owned(), url.trim().to_owned()));
    }
    fn build(flat: &[(usize, String, String)], at: &mut usize, depth: usize) -> Vec<Value> {
        let mut out = Vec::new();
        while *at < flat.len() && flat[*at].0 == depth {
            let (_, title, url) = &flat[*at];
            *at += 1;
            let mut item = json!({ "title": title, "url": url });
            let kids = build(flat, at, depth + 1);
            if !kids.is_empty() {
                item["items"] = Value::Array(kids);
            }
            out.push(item);
        }
        out
    }
    let mut at = 0;
    Ok(Value::Array(build(&flat, &mut at, 0)))
}

/// Menus, by title.
#[get("/admin/{shop}/menus", tag = "admin")]
pub(crate) async fn menus(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let list = page!(cx.get("/api/menus?limit=200&sort=title").await);
    let rows: Vec<Value> = list["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            json!({
                "id": m["id"], "title": m["title"], "handle": m["handle"],
                "count": m["items"].as_array().map_or(0, Vec::len),
            })
        })
        .collect();
    tab(&mut cx, "menus");
    cx.set("page_title", json!("Menus"));
    cx.set("menus", Value::Array(rows));
    Ok(cx.render("menus", StatusCode::OK))
}

/// The menu form's fields.
#[model]
#[derive(Default)]
pub struct MenuForm {
    pub title: Option<String>,
    pub handle: Option<String>,
    /// See [`menu_text`].
    pub items: Option<String>,
}

fn menu_form(mut cx: Ctx, menu: Value, error: Option<String>, status: StatusCode) -> Response {
    let creating = menu["id"].is_null();
    let title = if creating {
        "New menu".to_owned()
    } else {
        menu["title"].as_str().unwrap_or("Menu").to_owned()
    };
    tab(&mut cx, "menus");
    cx.set("page_title", json!(title));
    cx.set("menu", menu);
    cx.set("creating", json!(creating));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("menu", status)
}

/// The form for a new menu. The theme shows the menus `main` and
/// `footer`.
#[get("/admin/{shop}/menus/new", tag = "admin")]
pub(crate) async fn new_menu(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let blank = json!({ "title": "", "handle": "", "items": "Shop all | /collections/all\n" });
    Ok(menu_form(cx, blank, None, StatusCode::OK))
}

/// A menu's form.
#[get("/admin/{shop}/menus/{id}", tag = "admin")]
pub(crate) async fn edit_menu(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let mut m = page!(cx.get(&format!("/api/menus/{}", encode(&id))).await);
    m["items"] = json!(menu_text(&m["items"]));
    cx.set("saved", json!(q.contains_key("saved")));
    Ok(menu_form(cx, m, None, StatusCode::OK))
}

/// Create a menu (a form post).
#[post("/admin/{shop}/menus", tag = "admin")]
pub(crate) async fn create_menu(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<MenuForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    Ok(save_menu(cx, None, f).await)
}

/// Replace a menu (a form post).
#[post("/admin/{shop}/menus/{id}", tag = "admin")]
pub(crate) async fn update_menu(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<MenuForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    Ok(save_menu(cx, Some(id), f).await)
}

async fn save_menu(cx: Ctx, id: Option<String>, f: MenuForm) -> Response {
    let shop = cx.shop.clone();
    let creating = id.is_none();
    let id = id.unwrap_or_default();
    let mut typed = json!({
        "title": text(&f.title), "handle": text(&f.handle),
        "items": f.items.clone().unwrap_or_default(),
    });
    if !creating {
        typed["id"] = json!(id);
    }
    let items = match menu_items(&f.items.clone().unwrap_or_default()) {
        Ok(i) => i,
        Err(e) => return menu_form(cx, typed, Some(e), StatusCode::UNPROCESSABLE_ENTITY),
    };
    let handle = text(&f.handle);
    let body = json!({
        "title": text(&f.title), "items": items,
        "handle": (!handle.is_empty()).then_some(handle),
    });
    let (status, out) = if creating {
        cx.call(Method::POST, "/api/menus", Some(&body)).await
    } else {
        cx.call(
            Method::PUT,
            &format!("/api/menus/{}", encode(&id)),
            Some(&body),
        )
        .await
    };
    match status {
        200 | 201 => {
            let id = out["id"].as_str().unwrap_or_default();
            Redirect::to(&format!("/admin/{shop}/menus/{}?saved=1", encode(id))).into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            menu_form(cx, typed, Some(message(&out)), code)
        }
        _ => cx.failed(status, &out),
    }
}

/// URL redirects, and a form to add one.
#[get("/admin/{shop}/redirects", tag = "admin")]
pub(crate) async fn redirects(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    Ok(redirect_list(cx, None, None, StatusCode::OK).await)
}

async fn redirect_list(
    mut cx: Ctx,
    typed: Option<Value>,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let list = page_or!(cx.get("/api/redirects?limit=200").await);
    tab(&mut cx, "redirects");
    cx.set("page_title", json!("Redirects"));
    cx.set("redirects", list["items"].clone());
    cx.set("typed", typed.unwrap_or_else(|| json!({})));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("redirects", status)
}

/// The redirect form's fields.
#[model]
pub struct RedirectForm {
    pub from_path: String,
    pub to_url: String,
}

/// Add a redirect (a form post).
#[post("/admin/{shop}/redirects", tag = "admin")]
pub(crate) async fn add_redirect(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<RedirectForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "content").await);
    let body = json!({ "from_path": f.from_path.trim(), "to_url": f.to_url.trim() });
    let (status, out) = cx.call(Method::POST, "/api/redirects", Some(&body)).await;
    Ok(match status {
        201 => Redirect::to(&format!("/admin/{shop}/redirects")).into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            redirect_list(cx, Some(body), Some(message(&out)), code).await
        }
        _ => cx.failed(status, &out),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_read_as_lines() {
        let items = json!([
            { "title": "Shop", "url": "/collections/all", "items": [
                { "title": "Shoes", "url": "/collections/shoes", "items": [
                    { "title": "Trail", "url": "/collections/trail" }
                ]}
            ]},
            { "title": "About us", "url": "/pages/about" }
        ]);
        let text = menu_text(&items);
        assert_eq!(
            text,
            "Shop | /collections/all\n- Shoes | /collections/shoes\n-- Trail | /collections/trail\nAbout us | /pages/about\n"
        );
        assert_eq!(menu_items(&text).unwrap(), items);
        assert_eq!(menu_items("\n  \n").unwrap(), json!([]));
        assert!(menu_items("Shop /x").unwrap_err().contains("Line 1"));
        assert!(menu_items("- Shop | /x").is_err());
        assert!(menu_items("A | /a\n-- B | /b")
            .unwrap_err()
            .contains("Line 2"));
    }
}
