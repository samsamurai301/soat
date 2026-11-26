//! Gift cards and store credit: issue them (a gift card's code is shown
//! once), see their ledgers, add to or take from a balance, disable them.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

async fn currency(cx: &Ctx) -> String {
    let (_, shop) = cx.call(Method::GET, "/api/shop", None).await;
    shop["currency"].as_str().unwrap_or("EUR").to_owned()
}

/// A card with its amounts as money.
fn shown(c: &Value) -> Value {
    let cur = c["currency"].as_str().unwrap_or("EUR");
    let m = |v: &Value| money(v.as_i64().unwrap_or(0), cur);
    let mut c = c.clone();
    c["balance"] = json!(m(&c["balance_minor"]));
    c["initial"] = json!(m(&c["initial_minor"]));
    c["store_credit"] = json!(c["kind"] == "store_credit");
    if let Some(t) = c["transactions"].as_array_mut() {
        for t in t {
            t["amount"] = json!(m(&t["amount_minor"]));
            t["after"] = json!(m(&t["balance_minor"]));
        }
    }
    c
}

/// The shop's gift cards and store credit.
#[get("/admin/{shop}/gift-cards", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let page = page!(cx.get("/api/gift-cards?limit=200").await);
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(shown)
        .collect();
    cx.set("cards", Value::Array(items));
    cx.set("page_title", json!("Gift cards"));
    Ok(cx.render("giftcards", StatusCode::OK))
}

async fn new_form(
    mut cx: Ctx,
    typed: Value,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let currency = currency(&cx).await;
    cx.set("currency", json!(currency));
    cx.set("form", typed);
    cx.set("page_title", json!("Issue a gift card"));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("giftcard_new", status)
}

/// The form to issue a gift card or store credit.
#[get("/admin/{shop}/gift-cards/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    Ok(new_form(cx, json!({ "kind": "gift_card" }), None, StatusCode::OK).await)
}

async fn show(
    mut cx: Ctx,
    id: &str,
    code: Option<String>,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let card = page_or!(cx.get(&format!("/api/gift-cards/{}", encode(id))).await);
    if let Some(c) = card["customer_id"].as_str() {
        let (st, customer) = cx
            .call(Method::GET, &format!("/api/customers/{}", encode(c)), None)
            .await;
        if st == 200 {
            cx.set("customer", customer);
        }
    }
    cx.set(
        "page_title",
        json!(if card["kind"] == "store_credit" {
            "Store credit".to_owned()
        } else {
            format!(
                "Gift card ending in {}",
                card["last_chars"].as_str().unwrap_or("")
            )
        }),
    );
    cx.set(
        "expires",
        json!(card["expires_at"]
            .as_str()
            .map(|t| t.chars().take(10).collect::<String>())
            .unwrap_or_default()),
    );
    cx.set("card", shown(&card));
    if let Some(code) = code {
        cx.set("code", json!(code));
    }
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("giftcard", status)
}

/// Issue a gift card or store credit (a form post); a gift card's code is
/// shown on the page that follows, once.
#[post("/admin/{shop}/gift-cards", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let f = Fields::parse(&raw);
    let currency = currency(&cx).await;
    let typed = json!({
        "kind": f.get("kind"), "amount": f.get("amount"), "code": f.get("code"),
        "email": f.get("email"), "expires": f.get("expires"), "note": f.get("note"),
        "is_store_credit": f.get("kind") == "store_credit",
    });
    let fail =
        |cx, e: String| new_form(cx, typed.clone(), Some(e), StatusCode::UNPROCESSABLE_ENTITY);
    let Some(minor) = amount(&f.get("amount"), &currency) else {
        return Ok(fail(cx, format!("Give the amount in {currency}, like 25.00.")).await);
    };
    let kind = if f.get("kind") == "store_credit" {
        "store_credit"
    } else {
        "gift_card"
    };
    let email = f.get("email").to_lowercase();
    let mut customer = Value::Null;
    if !email.is_empty() {
        let found = page!(
            cx.get(&format!("/api/customers?q={}&limit=20", encode(&email)))
                .await
        );
        match found["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["email"].as_str() == Some(email.as_str()))
        {
            Some(c) => customer = c["id"].clone(),
            None => return Ok(fail(cx, format!("No customer has the email {email}.")).await),
        }
    } else if kind == "store_credit" {
        return Ok(fail(
            cx,
            "Store credit is for a customer: give their email.".into(),
        )
        .await);
    }
    let code = f.get("code");
    let expires = f.get("expires");
    let note = f.get("note");
    let body = json!({
        "kind": kind, "amount_minor": minor, "currency": currency,
        "code": if code.is_empty() || kind == "store_credit" { Value::Null } else { json!(code) },
        "customer_id": customer,
        "expires_at": if expires.is_empty() { Value::Null } else { json!(format!("{expires}T23:59:59Z")) },
        "note": note,
    });
    let (status, out) = cx.call(Method::POST, "/api/gift-cards", Some(&body)).await;
    Ok(match status {
        201 => {
            let id = out["id"].as_str().unwrap_or_default().to_owned();
            let code = out["code"].as_str().map(str::to_owned);
            show(cx, &id, code, None, StatusCode::CREATED).await
        }
        401 => crate::to_login(),
        409 | 422 => fail(cx, message(&out)).await,
        _ => cx.failed(status, &out),
    })
}

/// A gift card's or store credit's page with its ledger.
#[get("/admin/{shop}/gift-cards/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    Ok(show(cx, &id, None, None, StatusCode::OK).await)
}

/// Adjust the balance, save the note and end, or disable and enable the
/// card (form posts).
#[post("/admin/{shop}/gift-cards/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "discounts").await);
    let f = Fields::parse(&raw);
    let path = format!("/api/gift-cards/{}", encode(&id));
    let (status, out) = match action.as_str() {
        "adjust" => {
            let (_, card) = cx.call(Method::GET, &path, None).await;
            let cur = card["currency"].as_str().unwrap_or("EUR").to_owned();
            let typed = f.get("amount");
            let (negative, digits) = match typed.strip_prefix('-') {
                Some(rest) => (true, rest.to_owned()),
                None => (false, typed.trim_start_matches('+').to_owned()),
            };
            let Some(minor) = amount(&digits, &cur).filter(|m| *m > 0) else {
                let e = format!("Give an amount in {cur} to add, or with a minus to take away.");
                return Ok(show(cx, &id, None, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            };
            let minor = if negative { -minor } else { minor };
            cx.call(
                Method::POST,
                &format!("{path}/adjustments"),
                Some(&json!({ "amount_minor": minor, "note": f.get("note") })),
            )
            .await
        }
        "save" => {
            let expires = f.get("expires");
            let body = json!({
                "note": f.get("note"),
                "expires_at": if expires.is_empty() { String::new() } else { format!("{expires}T23:59:59Z") },
            });
            cx.call(Method::PATCH, &path, Some(&body)).await
        }
        "disable" | "enable" => {
            let body = json!({ "active": action == "enable" });
            cx.call(Method::PATCH, &path, Some(&body)).await
        }
        _ => return Err(Error::not_found()),
    };
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/gift-cards/{}", encode(&id))).into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            show(
                cx,
                &id,
                None,
                Some(message(&out)),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        _ => cx.failed(status, &out),
    })
}
