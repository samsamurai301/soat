//! B2B: companies (their people, prices, terms and what they owe) and
//! quotes, answered with an offer.

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

fn amount(currency: &str, v: &Value) -> Value {
    match v.as_i64() {
        Some(m) => json!(money(m, currency)),
        None => Value::Null,
    }
}

/// The shop's companies and a form for a new one.
#[get("/admin/{shop}/companies", tag = "admin")]
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
    let (code, page) = cx.call(Method::GET, "/api/companies", None).await;
    match code {
        200 => {}
        404 => {
            cx.set("missing", json!(true));
            cx.set("page_title", json!("Companies"));
            return cx.render("companies", StatusCode::OK);
        }
        _ => return cx.failed(code, &page),
    }
    cx.set("companies", page["items"].clone());
    let (_, asked) = cx
        .call(Method::GET, "/api/quotes?status=requested", None)
        .await;
    cx.set(
        "waiting",
        json!(asked["items"].as_array().map_or(0, Vec::len)),
    );
    cx.set("typed", typed);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("page_title", json!("Companies"));
    cx.render("companies", status)
}

/// The company form as an API body; a bad amount is an error.
fn body(f: &Fields, currency: &str) -> std::result::Result<Value, String> {
    let c = Currency::parse(currency).map_err(|_| "Choose a currency like EUR.".to_owned())?;
    let limit = f.get("credit_limit");
    let limit = limit.trim();
    let limit = if limit.is_empty() {
        json!(-1)
    } else {
        json!(c
            .parse_decimal(limit)
            .filter(|l| *l >= 0)
            .ok_or_else(|| format!("The credit limit is an amount in {currency}, like 5000."))?)
    };
    let terms = f.get("payment_terms_days");
    Ok(json!({
        "name": f.get("name").trim(),
        "vat_number": f.get("vat_number"),
        "price_tag": f.get("price_tag"),
        "payment_terms_days": terms.trim().parse::<i64>().unwrap_or(0),
        "credit_limit_minor": limit,
        "note": f.get("note"),
    }))
}

/// Add a company (a form post).
#[post("/admin/{shop}/companies", tag = "admin")]
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
    let currency = {
        let c = f.get("currency");
        let c = c.trim().to_uppercase();
        if c.is_empty() {
            "EUR".to_owned()
        } else {
            c
        }
    };
    let mut b = match body(&f, &currency) {
        Ok(b) => b,
        Err(e) => {
            let typed = json!({ "name": f.get("name") });
            return Ok(list_page(cx, Some(e), typed, StatusCode::UNPROCESSABLE_ENTITY).await);
        }
    };
    b["currency"] = json!(currency);
    if b["credit_limit_minor"] == -1 {
        b["credit_limit_minor"] = Value::Null;
    }
    let (status, out) = cx.call(Method::POST, "/api/companies", Some(&b)).await;
    Ok(match status {
        201 => {
            let id = out["id"].as_str().unwrap_or_default();
            Redirect::to(&format!("/admin/{shop}/companies/{}", encode(id))).into_response()
        }
        401 => crate::to_login(),
        422 => list_page(cx, Some(message(&out)), b, StatusCode::UNPROCESSABLE_ENTITY).await,
        _ => cx.failed(status, &out),
    })
}

async fn show(mut cx: Ctx, id: &str, notice: Option<&str>, error: Option<String>) -> Response {
    let path = format!("/api/companies/{}", encode(id));
    let mut c = page_or!(cx.get(&path).await);
    let currency = c["currency"].as_str().unwrap_or("EUR").to_owned();
    if let Ok(cur) = Currency::parse(&currency) {
        if let Some(l) = c["credit_limit_minor"].as_i64() {
            c["credit_limit"] = json!(cur.decimal(l));
        }
    }
    c["on_hold"] = json!(c["status"] == "on_hold");
    let members = page_or!(cx.get(&format!("{path}/members")).await);
    let mut b = page_or!(cx.get(&format!("{path}/balance")).await);
    for k in [
        "open_minor",
        "overdue_minor",
        "credit_limit_minor",
        "available_minor",
    ] {
        let shown = amount(&currency, &b[k]);
        b[k.trim_end_matches("_minor")] = shown;
    }
    if let Some(orders) = b["orders"].as_array_mut() {
        for o in orders {
            o["total"] = amount(&currency, &o["total_minor"]);
            o["placed"] = json!(o["placed_at"].as_str().and_then(|d| d.get(..10)));
            o["due"] = json!(o["due_at"].as_str().and_then(|d| d.get(..10)));
            o["overdue"] = json!(o["state"] == "overdue");
        }
    }
    cx.set("page_title", c["name"].clone());
    cx.set("company", c);
    cx.set("members", members["items"].clone());
    cx.set("balance", b);
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
    cx.render("company", status)
}

/// The company page's query.
#[model]
pub struct CompanyQuery {
    pub done: Option<String>,
}

/// A company: its details, its people and what it owes.
#[get("/admin/{shop}/companies/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    Query(q): Query<CompanyQuery>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let notice = q.done.is_some().then_some("Saved.");
    Ok(show(cx, &id, notice, None).await)
}

/// Save a company, put it on hold or back, add or remove a person (form
/// posts).
#[post("/admin/{shop}/companies/{id}/{action}", tag = "admin")]
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
    let path = format!("/api/companies/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "save" => {
            let (_, current) = cx.call(Method::GET, &path, None).await;
            let currency = current["currency"].as_str().unwrap_or("EUR");
            match body(&f, currency) {
                Ok(b) => cx.call(Method::PATCH, &path, Some(&b)).await,
                Err(e) => return Ok(show(cx, &id, None, Some(e)).await),
            }
        }
        "hold" | "release" => {
            let status = if action == "hold" {
                "on_hold"
            } else {
                "active"
            };
            cx.call(Method::PATCH, &path, Some(&json!({ "status": status })))
                .await
        }
        "add" => {
            let email = f.get("email");
            let (_, found) = cx
                .call(
                    Method::GET,
                    &format!("/api/customers?q={}", encode(email.trim())),
                    None,
                )
                .await;
            let customer = found["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| {
                    c["email"]
                        .as_str()
                        .is_some_and(|e| e.eq_ignore_ascii_case(email.trim()))
                })
                .and_then(|c| c["id"].as_str())
                .map(str::to_owned);
            let Some(customer) = customer else {
                let e = format!("No customer has the email {}.", email.trim());
                return Ok(show(cx, &id, None, Some(e)).await);
            };
            let role = if f.get("role") == "admin" {
                "admin"
            } else {
                "buyer"
            };
            cx.call(
                Method::PUT,
                &format!("{path}/members/{}", encode(&customer)),
                Some(&json!({ "role": role })),
            )
            .await
        }
        "remove" => {
            let customer = f.get("customer_id");
            cx.call(
                Method::DELETE,
                &format!("{path}/members/{}", encode(&customer)),
                None,
            )
            .await
        }
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 | 204 => Redirect::to(&format!(
            "/admin/{shop}/companies/{}?done=saved",
            encode(&id)
        ))
        .into_response(),
        401 => crate::to_login(),
        404 | 409 | 422 => show(cx, &id, None, Some(message(&out))).await,
        _ => cx.failed(status, &out),
    })
}

/// The quotes page's query.
#[model]
pub struct QuotesQuery {
    pub status: Option<String>,
}

/// The shop's quotes, waiting ones first by default.
#[get("/admin/{shop}/quotes", tag = "admin")]
pub(crate) async fn quotes(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    Query(q): Query<QuotesQuery>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let filter = q
        .status
        .as_deref()
        .filter(|s| ["requested", "offered", "accepted", "declined"].contains(s));
    let path = match filter {
        Some(f) => format!("/api/quotes?status={f}"),
        None => "/api/quotes".to_owned(),
    };
    let (code, page) = cx.call(Method::GET, &path, None).await;
    match code {
        200 => {}
        404 => cx.set("missing", json!(true)),
        _ => return Ok(cx.failed(code, &page)),
    }
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| {
            let mut q = q.clone();
            q["date"] = json!(q["created_at"].as_str().and_then(|d| d.get(..10)));
            q["first"] = q["lines"][0]["title"].clone();
            q["count"] = json!(q["lines"].as_array().map_or(0, Vec::len));
            q
        })
        .collect();
    cx.set("quotes", Value::Array(items));
    cx.set("status", json!(filter));
    cx.set("page_title", json!("Quotes"));
    Ok(cx.render("quotes", StatusCode::OK))
}

async fn show_quote(mut cx: Ctx, id: &str, error: Option<String>) -> Response {
    let mut q = page_or!(cx.get(&format!("/api/quotes/{}", encode(id))).await);
    let open = q["status"] == "requested" || q["status"] == "offered";
    q["open"] = json!(open);
    if let Some(d) = q["offer"].as_object().cloned() {
        let currency = d["currency"].as_str().unwrap_or("EUR").to_owned();
        let mut offer = Value::Object(d);
        for k in [
            "subtotal_minor",
            "discount_minor",
            "shipping_price_minor",
            "tax_minor",
            "total_minor",
        ] {
            let shown = amount(&currency, &offer[k]);
            offer[k.trim_end_matches("_minor")] = shown;
        }
        if let Some(lines) = offer["lines"].as_array_mut() {
            for l in lines {
                l["unit_price"] = amount(&currency, &l["unit_price_minor"]);
                if let Ok(c) = Currency::parse(&currency) {
                    l["unit_price_input"] =
                        json!(c.decimal(l["unit_price_minor"].as_i64().unwrap_or(0)));
                }
            }
        }
        q["offer"] = offer;
    }
    // The offer form starts from the offer, else from the request.
    let form_lines: Vec<Value> = match q["offer"]["lines"].as_array() {
        Some(lines) => lines
            .iter()
            .map(|l| {
                json!({ "item_id": l["item_id"], "title": l["title"], "sku": l["sku"],
                              "quantity": l["quantity"], "price": l["unit_price_input"] })
            })
            .collect(),
        None => q["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                json!({ "item_id": l["item_id"], "title": l["title"], "sku": l["sku"],
                              "quantity": l["quantity"], "price": "" })
            })
            .collect(),
    };
    let form_lines: Vec<Value> = form_lines
        .into_iter()
        .enumerate()
        .map(|(n, mut l)| {
            l["n"] = json!(n);
            l
        })
        .collect();
    let customer = q["customer_id"].as_str().unwrap_or_default().to_owned();
    let (_, c) = cx
        .call(
            Method::GET,
            &format!("/api/customers/{}", encode(&customer)),
            None,
        )
        .await;
    q["customer_email"] = c["email"].clone();
    if let Some(company) = q["company_id"].as_str() {
        let (_, co) = cx
            .call(
                Method::GET,
                &format!("/api/companies/{}", encode(company)),
                None,
            )
            .await;
        q["company_name"] = co["name"].clone();
    }
    cx.set("page_title", json!("Quote"));
    cx.set("quote", q);
    cx.set("form_lines", Value::Array(form_lines));
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("quote", status)
}

/// A quote: what the buyer asked for, and the offer.
#[get("/admin/{shop}/quotes/{id}", tag = "admin")]
pub(crate) async fn quote(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    Ok(show_quote(cx, &id, None).await)
}

/// Send an offer for a quote, or decline it (form posts). The offer form
/// has `item_N`, `quantity_N` and `price_N` per line (a blank price is
/// the buyer's own), `shipping`, `discount`, `days` and `reply`.
#[post("/admin/{shop}/quotes/{id}/{action}", tag = "admin")]
pub(crate) async fn quote_act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "customers").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/quotes/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "offer" => {
            let currency = f.get("currency");
            let c = Currency::parse(currency.trim()).unwrap_or(
                Currency::parse("EUR").map_err(|_| Error::internal("EUR is a currency"))?,
            );
            let money_of = |k: &str| -> std::result::Result<Option<i64>, String> {
                let t = f.get(k);
                let t = t.trim();
                if t.is_empty() {
                    return Ok(None);
                }
                c.parse_decimal(t)
                    .filter(|m| *m >= 0)
                    .map(Some)
                    .ok_or_else(|| format!("{t} is not an amount in {}.", c.as_str()))
            };
            let mut lines = Vec::new();
            let mut bad = None;
            for n in 0..100 {
                let item = f.get(&format!("item_{n}"));
                if item.is_empty() {
                    break;
                }
                let quantity = f
                    .get(&format!("quantity_{n}"))
                    .trim()
                    .parse::<i64>()
                    .unwrap_or(0);
                if quantity == 0 {
                    continue;
                }
                match money_of(&format!("price_{n}")) {
                    Ok(p) => lines.push(
                        json!({ "item_id": item, "quantity": quantity, "unit_price_minor": p }),
                    ),
                    Err(e) => bad = Some(e),
                }
            }
            let shipping = money_of("shipping");
            let discount = money_of("discount");
            let (shipping, discount) = match (bad, shipping, discount) {
                (Some(e), _, _) | (None, Err(e), _) | (None, _, Err(e)) => {
                    return Ok(show_quote(cx, &id, Some(e)).await)
                }
                (None, Ok(s), Ok(d)) => (s, d),
            };
            let days = f.get("days").trim().parse::<i64>().unwrap_or(30);
            let b = json!({ "lines": lines, "shipping_price_minor": shipping,
                            "discount_minor": discount, "expires_in_days": days,
                            "reply": f.get("reply") });
            cx.call(Method::POST, &format!("{path}/offer"), Some(&b))
                .await
        }
        "decline" => {
            let b = json!({ "reply": f.get("reply") });
            cx.call(Method::POST, &format!("{path}/decline"), Some(&b))
                .await
        }
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/quotes/{}", encode(&id))).into_response(),
        401 => crate::to_login(),
        409 | 422 => show_quote(cx, &id, Some(message(&out))).await,
        _ => cx.failed(status, &out),
    })
}
