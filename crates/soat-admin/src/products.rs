//! Products: a searchable list, and a form to create, change or delete
//! one.

use serde_json::{json, Map, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

const PER_PAGE: i64 = 50;

/// The list's filters.
#[model]
pub struct ListQuery {
    pub q: Option<String>,
    pub status: Option<String>,
    pub offset: Option<i64>,
}

/// Products, 50 a page: `?q=` searches title, SKU and handle; `?status=`
/// filters.
#[get("/admin/{shop}/products", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(f): Query<ListQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let q = f.q.unwrap_or_default().trim().to_owned();
    let status = f
        .status
        .filter(|s| ["active", "draft", "archived"].contains(&s.as_str()));
    let offset = f.offset.unwrap_or(0).max(0);
    let mut query = vec![
        ("limit", PER_PAGE.to_string()),
        ("offset", offset.to_string()),
        ("sort", "-id".to_owned()),
    ];
    if !q.is_empty() {
        query.push(("q", q.clone()));
    }
    if let Some(s) = &status {
        query.push(("status", s.clone()));
    }
    let qs = serde_urlencoded::to_string(&query).unwrap_or_default();
    let page = page!(cx.get(&format!("/api/products?{qs}")).await);
    let rows: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "id": p["id"],
                "title": p["title"],
                "sku": p["sku"],
                "status": p["status"],
                "active": p["status"] == "active",
                "price": money(
                    p["price_minor"].as_i64().unwrap_or(0),
                    p["currency"].as_str().unwrap_or_default(),
                ),
            })
        })
        .collect();
    let total = page["total"].as_i64().unwrap_or(0);
    let link = |at: i64| {
        let mut q2 = vec![("offset", at.to_string())];
        if !q.is_empty() {
            q2.push(("q", q.clone()));
        }
        if let Some(s) = &status {
            q2.push(("status", s.clone()));
        }
        format!(
            "/admin/{shop}/products?{}",
            serde_urlencoded::to_string(&q2).unwrap_or_default()
        )
    };
    if offset > 0 {
        cx.set("prev_url", json!(link((offset - PER_PAGE).max(0))));
    }
    if offset + PER_PAGE < total {
        cx.set("next_url", json!(link(offset + PER_PAGE)));
    }
    cx.set("page_title", json!("Products"));
    cx.set("products", Value::Array(rows));
    cx.set("total", json!(total));
    cx.set("q", json!(q));
    cx.set(
        "status",
        json!({ status.as_deref().unwrap_or("all"): true }),
    );
    Ok(cx.render("products", StatusCode::OK))
}

/// The product form's fields, as typed.
#[model]
#[derive(Default)]
pub struct ProductForm {
    pub title: Option<String>,
    pub sku: Option<String>,
    pub handle: Option<String>,
    pub status: Option<String>,
    /// e.g. `19.99`, in `currency`.
    pub price: Option<String>,
    /// Only when creating; fixed after.
    pub currency: Option<String>,
    pub description: Option<String>,
    /// Comma-separated.
    pub tags: Option<String>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    /// Checked for a digital product: nothing ships.
    pub digital: Option<String>,
}

fn text(v: &Option<String>) -> String {
    v.as_deref().map(str::trim).unwrap_or_default().to_owned()
}

impl ProductForm {
    /// The form as an API body; a price that is no amount is an error.
    fn body(&self, currency: &str, creating: bool) -> std::result::Result<Value, String> {
        let c = Currency::parse(currency).map_err(|_| "Choose a currency like EUR.".to_owned())?;
        let price = c
            .parse_decimal(&text(&self.price))
            .filter(|p| *p >= 0)
            .ok_or_else(|| format!("The price is an amount in {currency}, like 19.99."))?;
        let tags: Vec<String> = text(&self.tags)
            .split(',')
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let mut b = Map::new();
        b.insert("title".into(), json!(text(&self.title)));
        b.insert("sku".into(), json!(text(&self.sku)));
        b.insert("price_minor".into(), json!(price));
        b.insert(
            "status".into(),
            json!(self.status.as_deref().unwrap_or("draft")),
        );
        b.insert("description".into(), json!(text(&self.description)));
        b.insert("tags".into(), json!(tags));
        b.insert("seo_title".into(), json!(text(&self.seo_title)));
        b.insert("seo_description".into(), json!(text(&self.seo_description)));
        b.insert("requires_shipping".into(), json!(self.digital.is_none()));
        let handle = text(&self.handle);
        if !handle.is_empty() {
            b.insert("handle".into(), json!(handle));
        }
        if creating {
            b.insert("currency".into(), json!(currency));
        }
        Ok(Value::Object(b))
    }

    /// A product from the API as the form shows it.
    fn of(p: &Value) -> Value {
        let currency = p["currency"].as_str().unwrap_or_default();
        let price = Currency::parse(currency)
            .map(|c| c.decimal(p["price_minor"].as_i64().unwrap_or(0)))
            .unwrap_or_default();
        let tags: Vec<&str> = p["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        json!({
            "id": p["id"], "title": p["title"], "sku": p["sku"], "handle": p["handle"],
            "price": price, "currency": currency, "description": p["description"],
            "tags": tags.join(", "), "seo_title": p["seo_title"],
            "seo_description": p["seo_description"], "status": p["status"],
            "digital": p["requires_shipping"] == false,
        })
    }

    fn typed(&self, currency: &str) -> Value {
        json!({
            "title": text(&self.title), "sku": text(&self.sku), "handle": text(&self.handle),
            "price": text(&self.price), "currency": currency,
            "description": text(&self.description), "tags": text(&self.tags),
            "seo_title": text(&self.seo_title), "seo_description": text(&self.seo_description),
            "status": self.status.as_deref().unwrap_or("draft"),
            "digital": self.digital.is_some(),
        })
    }
}

fn show_form(mut cx: Ctx, product: Value, error: Option<String>, status: StatusCode) -> Response {
    let state = product["status"].as_str().unwrap_or("draft").to_owned();
    let creating = product["id"].is_null();
    cx.set(
        "page_title",
        json!(if creating {
            "New product".to_owned()
        } else {
            product["title"].as_str().unwrap_or("Product").to_owned()
        }),
    );
    cx.set("product", product);
    cx.set("creating", json!(creating));
    cx.set("state", json!({ state: true }));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("product", status)
}

/// The form for a new product; the currency starts as the shop's most
/// used one.
#[get("/admin/{shop}/products/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    // The shop's currency, else the latest product's, else euros.
    let (_, profile) = cx.call(Method::GET, "/api/shop", None).await;
    let recent = page!(cx.get("/api/products?limit=1&sort=-id").await);
    let currency = profile["currency"]
        .as_str()
        .or_else(|| recent["items"][0]["currency"].as_str())
        .unwrap_or("EUR");
    let form = ProductForm::default().typed(currency);
    Ok(show_form(cx, form, None, StatusCode::OK))
}

/// Create a product (a form post); problems show the form again, as typed.
#[post("/admin/{shop}/products", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<ProductForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let currency = text(&f.currency).to_ascii_uppercase();
    let body = match f.body(&currency, true) {
        Ok(b) => b,
        Err(e) => {
            let typed = f.typed(&currency);
            return Ok(show_form(
                cx,
                typed,
                Some(e),
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
    };
    let (status, out) = cx.call(Method::POST, "/api/products", Some(&body)).await;
    match status {
        201 => {
            let id = out["id"].as_str().unwrap_or_default();
            Ok(Redirect::to(&format!("/admin/{shop}/products/{id}?saved=1")).into_response())
        }
        401 => Ok(Redirect::to("/admin/login").into_response()),
        409 | 422 => {
            let typed = f.typed(&currency);
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            Ok(show_form(cx, typed, Some(message(&out)), code))
        }
        _ => Ok(cx.failed(status, &out)),
    }
}

/// A product's form.
#[get("/admin/{shop}/products/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let p = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
    cx.set("saved", json!(q.contains_key("saved")));
    if let Some(l) = q.get("translated").and_then(Value::as_str) {
        cx.set("translated", json!(l));
    }
    Ok(page(cx, &p, None, StatusCode::OK).await)
}

/// A product's page: its form, options, variants, media and the writing
/// helpers.
pub(crate) async fn page(
    mut cx: Ctx,
    p: &Value,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    languages(&mut cx).await;
    crate::variants::load(&mut cx, p).await;
    if let Some(id) = p["id"].as_str() {
        crate::apps::links(&mut cx, "product", Some(id)).await;
    }
    show_form(cx, ProductForm::of(p), error, status)
}

/// The shop's other languages, to translate into; none when the staff
/// member may not see them.
async fn languages(cx: &mut Ctx) {
    let (status, l) = cx.call(Method::GET, "/api/languages", None).await;
    if status == 200 {
        let codes: Vec<Value> = l["languages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| **c != l["default_language"])
            .map(|c| json!({ "code": c }))
            .collect();
        cx.set("languages", Value::Array(codes));
    }
}

/// The form's fields.
#[model]
pub struct WriteForm {
    pub notes: Option<String>,
    pub language: Option<String>,
}

/// Have Claude suggest the product's description and search-engine
/// fields (a form post): the form shows them, unsaved, to review.
#[post("/admin/{shop}/products/{id}/write", tag = "admin")]
pub(crate) async fn write(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<WriteForm>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let p = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
    let body = json!({ "resource": "product", "id": p["id"], "task": "copy", "notes": f.notes });
    let (status, out) = cx
        .call(Method::POST, "/api/assistant/write", Some(&body))
        .await;
    languages(&mut cx).await;
    let mut form = ProductForm::of(&p);
    match status {
        200 => {
            for field in ["description", "seo_title", "seo_description"] {
                form[field] = out[field].clone();
            }
            cx.set("suggested", json!(true));
            Ok(show_form(cx, form, None, StatusCode::OK))
        }
        401 => Ok(crate::to_login()),
        _ => {
            let e = crate::assistant::problem(status, &out);
            Ok(show_form(cx, form, Some(e), StatusCode::OK))
        }
    }
}

/// Have Claude translate the product into one of the shop's languages
/// and save it as the product's translation (a form post).
#[post("/admin/{shop}/products/{id}/translate", tag = "admin")]
pub(crate) async fn translate(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<WriteForm>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let p = page!(cx.get(&format!("/api/products/{}", encode(&id))).await);
    let language = f.language.unwrap_or_default().trim().to_ascii_lowercase();
    let body = json!({
        "resource": "product", "id": p["id"], "task": "translate", "language": language,
    });
    let (mut status, mut out) = cx
        .call(Method::POST, "/api/assistant/write", Some(&body))
        .await;
    if status == 200 {
        let fields = json!({ "fields": {
            "title": out["title"], "description": out["description"],
            "seo_title": out["seo_title"], "seo_description": out["seo_description"],
        } });
        let path = format!(
            "/api/translations/product/{}/{}",
            encode(&id),
            encode(&language)
        );
        (status, out) = cx.call(Method::PUT, &path, Some(&fields)).await;
    }
    match status {
        200 => Ok(Redirect::to(&format!(
            "/admin/{shop}/products/{}?translated={}",
            encode(&id),
            encode(&language)
        ))
        .into_response()),
        401 => Ok(crate::to_login()),
        _ => {
            languages(&mut cx).await;
            let e = crate::assistant::problem(status, &out);
            Ok(show_form(cx, ProductForm::of(&p), Some(e), StatusCode::OK))
        }
    }
}

/// Change a product (a form post); problems show the form again, as typed.
#[post("/admin/{shop}/products/{id}", tag = "admin")]
pub(crate) async fn update(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<ProductForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let path = format!("/api/products/{}", encode(&id));
    let current = page!(cx.get(&path).await);
    let currency = current["currency"].as_str().unwrap_or_default().to_owned();
    let mut typed = f.typed(&currency);
    typed["id"] = current["id"].clone();
    let body = match f.body(&currency, false) {
        Ok(b) => b,
        Err(e) => {
            return Ok(show_form(
                cx,
                typed,
                Some(e),
                StatusCode::UNPROCESSABLE_ENTITY,
            ))
        }
    };
    let (status, out) = cx.call(Method::PATCH, &path, Some(&body)).await;
    match status {
        200 => Ok(
            Redirect::to(&format!("/admin/{shop}/products/{}?saved=1", encode(&id)))
                .into_response(),
        ),
        401 => Ok(Redirect::to("/admin/login").into_response()),
        409 | 422 => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            Ok(show_form(cx, typed, Some(message(&out)), code))
        }
        _ => Ok(cx.failed(status, &out)),
    }
}

/// Delete a product (a form post).
#[post("/admin/{shop}/products/{id}/delete", tag = "admin")]
pub(crate) async fn delete(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let (status, out) = cx
        .call(
            Method::DELETE,
            &format!("/api/products/{}", encode(&id)),
            None,
        )
        .await;
    match status {
        204 => Ok(Redirect::to(&format!("/admin/{shop}/products")).into_response()),
        401 => Ok(Redirect::to("/admin/login").into_response()),
        _ => Ok(cx.failed(status, &out)),
    }
}
