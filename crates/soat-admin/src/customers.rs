//! Customers: a searchable list, and a customer's page with their details,
//! addresses and orders.

use serde_json::{json, Map, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

const PER_PAGE: i64 = 50;

/// The list's filters.
#[model]
pub struct CustomerQuery {
    /// Searches email and names.
    pub q: Option<String>,
    pub tag: Option<String>,
    pub offset: Option<i64>,
}

/// Customers, newest first, 50 a page.
#[get("/admin/{shop}/customers", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(f): Query<CustomerQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let q = f.q.unwrap_or_default().trim().to_owned();
    let tag = f.tag.unwrap_or_default().trim().to_lowercase();
    let offset = f.offset.unwrap_or(0).max(0);
    let mut query = vec![
        ("limit", PER_PAGE.to_string()),
        ("offset", offset.to_string()),
    ];
    if !q.is_empty() {
        query.push(("q", q.clone()));
    }
    if !tag.is_empty() {
        query.push(("tag", tag.clone()));
    }
    let qs = serde_urlencoded::to_string(&query).unwrap_or_default();
    let page = page!(cx.get(&format!("/api/customers?{qs}")).await);
    let rows: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let name = format!(
                "{} {}",
                c["first_name"].as_str().unwrap_or_default(),
                c["last_name"].as_str().unwrap_or_default()
            );
            json!({
                "id": c["id"], "email": c["email"], "name": name.trim(),
                "tags": c["tags"], "account": c["has_account"],
                "marketing": c["accepts_marketing"],
            })
        })
        .collect();
    let total = page["total"].as_i64().unwrap_or(0);
    let link = |at: i64| {
        let mut q2 = vec![("offset", at.to_string())];
        if !q.is_empty() {
            q2.push(("q", q.clone()));
        }
        if !tag.is_empty() {
            q2.push(("tag", tag.clone()));
        }
        format!(
            "/admin/{shop}/customers?{}",
            serde_urlencoded::to_string(&q2).unwrap_or_default()
        )
    };
    if offset > 0 {
        cx.set("prev_url", json!(link((offset - PER_PAGE).max(0))));
    }
    if offset + PER_PAGE < total {
        cx.set("next_url", json!(link(offset + PER_PAGE)));
    }
    cx.set("page_title", json!("Customers"));
    cx.set("customers", Value::Array(rows));
    cx.set("total", json!(total));
    cx.set("q", json!(q));
    cx.set("tag", json!(tag));
    Ok(cx.render("customers", StatusCode::OK))
}

/// The customer form's fields, as typed.
#[model]
#[derive(Default)]
pub struct CustomerForm {
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
    /// Present when the box is ticked.
    pub accepts_marketing: Option<String>,
    /// Comma-separated.
    pub tags: Option<String>,
    pub note: Option<String>,
}

fn text(v: &Option<String>) -> String {
    v.as_deref().map(str::trim).unwrap_or_default().to_owned()
}

impl CustomerForm {
    fn body(&self) -> Value {
        let tags: Vec<String> = text(&self.tags)
            .split(',')
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        json!({
            "email": text(&self.email), "first_name": text(&self.first_name),
            "last_name": text(&self.last_name), "phone": text(&self.phone),
            "accepts_marketing": self.accepts_marketing.is_some(), "tags": tags,
            "note": text(&self.note),
        })
    }

    fn typed(&self) -> Value {
        let mut v = self.body();
        v["tags"] = json!(text(&self.tags));
        v
    }

    /// A customer from the API as the form shows it.
    fn of(c: &Value) -> Value {
        let tags: Vec<&str> = c["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut v = c.clone();
        v["tags"] = json!(tags.join(", "));
        v
    }
}

async fn show_form(
    mut cx: Ctx,
    customer: Value,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let creating = customer["id"].is_null();
    if let Some(id) = customer["id"].as_str() {
        crate::apps::links(&mut cx, "customer", Some(id)).await;
        let id = encode(id);
        let orders = page_or!(
            cx.get(&format!("/api/orders?customer_id={id}&limit=20"))
                .await
        );
        let orders: Vec<Value> = orders["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| {
                json!({
                    "id": o["id"], "number": o["number"],
                    "placed": o["placed_at"].as_str().and_then(|p| p.get(..10)),
                    "total": money(
                        o["total_minor"].as_i64().unwrap_or(0),
                        o["currency"].as_str().unwrap_or_default(),
                    ),
                    "canceled": o["status"] == "canceled",
                })
            })
            .collect();
        let addresses = page_or!(cx.get(&format!("/api/customers/{id}/addresses")).await);
        let addresses: Vec<Value> = addresses
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| {
                let s = |k: &str| a[k].as_str().unwrap_or_default().trim().to_owned();
                let line = [
                    format!("{} {}", s("first_name"), s("last_name"))
                        .trim()
                        .to_owned(),
                    s("company"),
                    s("line1"),
                    s("line2"),
                    format!("{} {}", s("postal_code"), s("city"))
                        .trim()
                        .to_owned(),
                    soat_core::address::country_name(&s("country"))
                        .unwrap_or_default()
                        .to_owned(),
                ]
                .into_iter()
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
                json!({ "line": line, "default": a["is_default"] })
            })
            .collect();
        cx.set("orders", Value::Array(orders));
        cx.set("addresses", Value::Array(addresses));
        // Loyalty points, when the shop has the module and a program.
        let (_, program) = cx.call(Method::GET, "/api/loyalty", None).await;
        if program["enabled"] == true {
            let (status, points) = cx
                .call(Method::GET, &format!("/api/loyalty/customers/{id}"), None)
                .await;
            if status == 200 {
                cx.set("points", points);
            }
        }
    }
    let name = format!(
        "{} {}",
        customer["first_name"].as_str().unwrap_or_default(),
        customer["last_name"].as_str().unwrap_or_default()
    );
    let title = if creating {
        "New customer".to_owned()
    } else if name.trim().is_empty() {
        customer["email"].as_str().unwrap_or("Customer").to_owned()
    } else {
        name.trim().to_owned()
    };
    cx.set("page_title", json!(title));
    cx.set("customer", customer);
    cx.set("creating", json!(creating));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("customer", status)
}

/// The form for a new customer.
#[get("/admin/{shop}/customers/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let blank = CustomerForm::default().typed();
    Ok(show_form(cx, blank, None, StatusCode::OK).await)
}

/// Add a customer (a form post); problems show the form again, as typed.
#[post("/admin/{shop}/customers", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Form(f): Form<CustomerForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let (status, out) = cx
        .call(Method::POST, "/api/customers", Some(&f.body()))
        .await;
    Ok(match status {
        201 => {
            let id = out["id"].as_str().unwrap_or_default();
            Redirect::to(&format!("/admin/{shop}/customers/{}?saved=1", encode(id))).into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            show_form(cx, f.typed(), Some(message(&out)), code).await
        }
        _ => cx.failed(status, &out),
    })
}

/// A customer's page.
#[get("/admin/{shop}/customers/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let c = page!(cx.get(&format!("/api/customers/{}", encode(&id))).await);
    cx.set("saved", json!(q.contains_key("saved")));
    Ok(show_form(cx, CustomerForm::of(&c), None, StatusCode::OK).await)
}

/// Change a customer's details (a form post).
#[post("/admin/{shop}/customers/{id}", tag = "admin")]
pub(crate) async fn update(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<CustomerForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let path = format!("/api/customers/{}", encode(&id));
    let (status, out) = cx.call(Method::PATCH, &path, Some(&f.body())).await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/customers/{}?saved=1", encode(&id)))
            .into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => {
            let mut typed = f.typed();
            typed["id"] = json!(id);
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            show_form(cx, typed, Some(message(&out)), code).await
        }
        _ => cx.failed(status, &out),
    })
}

/// The points form's fields.
#[model]
pub struct PointsForm {
    pub points: String,
    pub note: Option<String>,
}

/// Add loyalty points to a customer or take them away (a form post).
#[post("/admin/{shop}/customers/{id}/points", tag = "admin")]
pub(crate) async fn adjust_points(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Form(f): Form<PointsForm>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let Ok(points) = f.points.trim().parse::<i64>() else {
        return Ok(points_refused(cx, &id, "Give a whole number of points.".into()).await);
    };
    let body = json!({ "points": points, "note": f.note });
    let path = format!("/api/loyalty/customers/{}/adjustments", encode(&id));
    let (status, out) = cx.call(Method::POST, &path, Some(&body)).await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/customers/{}?saved=1", encode(&id)))
            .into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        409 | 422 => points_refused(cx, &id, message(&out)).await,
        _ => cx.failed(status, &out),
    })
}

async fn points_refused(cx: Ctx, id: &str, error: String) -> Response {
    let c = page_or!(cx.get(&format!("/api/customers/{}", encode(id))).await);
    show_form(
        cx,
        CustomerForm::of(&c),
        Some(error),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await
}

/// Delete a customer (a form post); their orders stay.
#[post("/admin/{shop}/customers/{id}/delete", tag = "admin")]
pub(crate) async fn delete(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let (status, out) = cx
        .call(
            Method::DELETE,
            &format!("/api/customers/{}", encode(&id)),
            None,
        )
        .await;
    Ok(match status {
        204 => Redirect::to(&format!("/admin/{shop}/customers")).into_response(),
        401 => Redirect::to("/admin/login").into_response(),
        _ => cx.failed(status, &out),
    })
}
