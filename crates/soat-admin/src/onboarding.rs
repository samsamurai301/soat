//! Getting started: the install's first admin, a new shop, and the welcome
//! steps that make a shop ready to sell.

use serde_json::{json, Value};
use soat_core::address::{country_name, country_names};
use soat_core::tax::is_eu;
use soat_core::TenantId;
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::message;
use crate::{session_cookie, to_login, token, Ctx, Shared};

/// Currencies offered first; any other code can be typed.
const CURRENCIES: [&str; 12] = [
    "EUR", "CHF", "GBP", "USD", "SEK", "DKK", "NOK", "PLN", "CZK", "HUF", "RON", "CAD",
];

/// The welcome steps, in order.
const STEPS: [&str; 5] = ["basics", "taxes", "payments", "shipping", "products"];

/// The form for the install's first admin.
#[get("/admin/setup", tag = "admin")]
pub(crate) async fn setup_page(s: State<Shared>) -> Result<Response> {
    Ok(s.pages.render(
        "setup",
        &json!({ "page_title": "Set up SOAT" }),
        StatusCode::OK,
    ))
}

/// The first admin's fields.
#[model]
pub struct SetupForm {
    pub setup_token: String,
    pub email: String,
    pub password: String,
}

/// Create the install's first admin (a form post) with the setup token
/// from the server's environment, sign them in, and go on to a new shop.
#[post("/admin/setup", tag = "admin")]
pub(crate) async fn setup(
    s: State<Shared>,
    db: Db,
    conn: ConnInfo,
    Form(f): Form<SetupForm>,
) -> Result<Response> {
    let body = json!({
        "setup_token": f.setup_token.trim(), "email": f.email.trim(), "password": f.password,
    });
    let (status, out) = s
        .api
        .send(
            &db,
            Method::POST,
            "/api/setup",
            Some(&body),
            None,
            None,
            conn,
        )
        .await;
    let error = match (status, out["token"].as_str()) {
        (201, Some(token)) => {
            let mut res = Redirect::to("/admin/new").into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                session_cookie(token, 30 * 86_400, conn.secure),
            );
            return Ok(res);
        }
        (403, _) => "That setup token is not the one this server was started with.".to_owned(),
        (409, _) => "This install is set up already. Sign in instead.".to_owned(),
        (422, _) => message(&out),
        _ => return Err(Error::internal("setting up failed")),
    };
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
    Ok(s.pages.render(
        "setup",
        &json!({ "page_title": "Set up SOAT", "email": f.email, "error": error }),
        code,
    ))
}

/// The new shop form.
#[get("/admin/new", tag = "admin")]
pub(crate) async fn new_shop_page(s: State<Shared>, headers: HeaderMap) -> Result<Response> {
    if token(&headers).is_none() {
        return Ok(to_login());
    }
    Ok(s.pages.render(
        "new_shop",
        &json!({ "page_title": "New shop" }),
        StatusCode::OK,
    ))
}

/// The new shop's fields.
#[model]
pub struct NewShopForm {
    pub name: String,
    /// Left empty, made from the name.
    pub id: Option<String>,
}

/// Create a shop (a form post; the install's admins only) and open its
/// welcome steps.
#[post("/admin", tag = "admin")]
pub(crate) async fn create_shop(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<NewShopForm>,
) -> Result<Response> {
    let Some(token) = token(&headers) else {
        return Ok(to_login());
    };
    let name = f.name.trim();
    let id = match f.id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_ascii_lowercase(),
        _ => slug(name),
    };
    let body = json!({ "id": id, "name": name });
    let (status, out) = s
        .api
        .send(
            &db,
            Method::POST,
            "/api/tenants",
            Some(&body),
            Some(&token),
            None,
            conn,
        )
        .await;
    let error = match status {
        201 => return Ok(Redirect::to(&format!("/admin/{id}/welcome")).into_response()),
        401 => return Ok(to_login()),
        403 => "Only the install's administrators can create shops.".to_owned(),
        409 => format!("The address “{id}” is taken. Choose another."),
        422 if out["code"] == "invalid_tenant" => format!(
            "“{id}” cannot be a shop's address. Use 2 to 63 small letters, digits and \
             dashes, starting with a letter."
        ),
        422 => message(&out),
        _ => return Err(Error::internal("creating the shop failed")),
    };
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
    Ok(s.pages.render(
        "new_shop",
        &json!({ "page_title": "New shop", "name": name, "id": f.id, "error": error }),
        code,
    ))
}

/// A shop address made from its name: `Café Ölberg` is `cafe-olberg`.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ß' => 's',
            c => c,
        };
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut id = out.trim_end_matches('-').to_owned();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id.insert_str(0, "shop-");
    }
    id.truncate(63);
    let mut id = id.trim_end_matches('-').to_owned();
    if TenantId::parse(&id).is_err() {
        id = format!("{}-shop", id.trim_end_matches('-'));
    }
    id
}

/// How far the shop is: each step done or not, from what is set up.
async fn progress(cx: &Ctx) -> Value {
    let get = |path: &'static str| async move {
        let (status, out) = cx.call(Method::GET, path, None).await;
        (status == 200).then_some(out)
    };
    let shop = get("/api/shop").await.unwrap_or_default();
    let count = |v: &Option<Value>| {
        v.as_ref().map_or(0, |v| {
            v["total"]
                .as_i64()
                .unwrap_or_else(|| v["items"].as_array().map_or(0, |a| a.len() as i64))
        })
    };
    let taxes = get("/api/tax/zones").await;
    let payments = get("/api/payment-methods").await;
    let shipping = get("/api/shipping/zones").await;
    let products = get("/api/products?limit=1").await;
    let samples = get("/api/products?tag=sample&limit=1").await;
    let done = [
        shop["country"].is_string() && shop["currency"].is_string(),
        count(&taxes) > 0,
        count(&payments) > 0,
        count(&shipping) > 0,
        count(&products) > 0,
    ];
    let steps: Value = STEPS
        .iter()
        .zip(done)
        .map(|(k, d)| (k.to_string(), json!(d)))
        .collect::<serde_json::Map<_, _>>()
        .into();
    json!({
        "shop": shop, "steps": steps,
        "done": done.iter().filter(|d| **d).count(), "total": STEPS.len(),
        "ready": done.iter().all(|d| *d),
        "samples": count(&samples),
    })
}

/// Whether a shop still has welcome steps to do.
pub(crate) async fn unfinished(cx: &Ctx) -> bool {
    progress(cx).await["ready"] != json!(true)
}

async fn welcome_page(mut cx: Ctx, error: Option<String>, status: StatusCode) -> Response {
    let p = progress(&cx).await;
    let country = p["shop"]["country"].as_str().unwrap_or_default().to_owned();
    let currency = p["shop"]["currency"].as_str().unwrap_or("EUR").to_owned();
    let mut countries: Vec<(&str, &str)> = country_names().to_vec();
    countries.sort_by_key(|(_, name)| *name);
    cx.set(
        "countries",
        json!(countries
            .iter()
            .map(|(c, n)| json!({ "code": c, "name": n, "selected": *c == country }))
            .collect::<Vec<_>>()),
    );
    let mut currencies: Vec<&str> = CURRENCIES.to_vec();
    if !currencies.contains(&currency.as_str()) {
        currencies.insert(0, &currency);
    }
    cx.set(
        "currencies",
        json!(currencies
            .iter()
            .map(|c| json!({ "code": c, "selected": *c == currency }))
            .collect::<Vec<_>>()),
    );
    cx.set("eu", json!(is_eu(&country)));
    cx.set("country_name", json!(country_name(&country)));
    cx.set("currency", json!(currency));
    cx.set("progress", p);
    cx.set("templates", json!(crate::samples::offered()));
    cx.set("page_title", json!("Set up your shop"));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("welcome", status)
}

/// The steps that make a shop ready to sell, and how far it is.
#[get("/admin/{shop}/welcome", tag = "admin")]
pub(crate) async fn welcome(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    Ok(welcome_page(cx, None, StatusCode::OK).await)
}

/// Do one welcome step (a form post): `basics` (name, country, currency),
/// `taxes` (the EU's VAT rates), `payments` (bank transfer), `shipping`
/// (one rate for the home country), `sample` (a store template's
/// products) and `remove-sample`.
#[post("/admin/{shop}/welcome/{step}", tag = "admin")]
pub(crate) async fn welcome_step(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, step)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "home").await);
    let f = Fields::parse(&body);
    let p = progress(&cx).await;
    let country = p["shop"]["country"].as_str().map(str::to_owned);
    let currency = p["shop"]["currency"].as_str().map(str::to_owned);
    match step.as_str() {
        "sample" => {
            let (Some(country), Some(currency)) = (&country, &currency) else {
                let e = "Choose the shop's country and currency first.".to_owned();
                return Ok(welcome_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            };
            return Ok(
                match crate::samples::load(&cx, &f.get("template"), currency, country).await {
                    Ok(_) => {
                        Redirect::to(&format!("/admin/{shop}/welcome#products")).into_response()
                    }
                    Err((status, out)) => step_failed(cx, status, &out).await,
                },
            );
        }
        "remove-sample" => {
            return Ok(match crate::samples::remove(&cx).await {
                Ok(()) => Redirect::to(&format!("/admin/{shop}/welcome#products")).into_response(),
                Err((status, out)) => step_failed(cx, status, &out).await,
            });
        }
        _ => {}
    }
    let calls: Vec<(Method, String, Value)> = match step.as_str() {
        "basics" => {
            let country = f.get("country").to_ascii_uppercase();
            let mut calls = vec![(
                Method::PATCH,
                "/api/shop".to_owned(),
                json!({
                    "name": Some(f.get("name")).filter(|n| !n.is_empty()),
                    "country": country, "currency": f.get("currency").to_ascii_uppercase(),
                }),
            )];
            // Tax is charged from where the shop is; the rest stays as set.
            let (status, mut tax) = cx.call(Method::GET, "/api/tax/settings", None).await;
            if status == 200 {
                tax["origin_country"] = json!(country);
                calls.push((Method::PUT, "/api/tax/settings".to_owned(), tax));
            }
            calls
        }
        "taxes" => vec![(
            Method::POST,
            "/api/tax/presets/eu-vat".to_owned(),
            json!({}),
        )],
        "payments" => vec![(
            Method::POST,
            "/api/payment-methods".to_owned(),
            json!({
                "provider": "manual", "name": "Bank transfer",
                "settings": { "instructions": f.get("instructions") },
            }),
        )],
        "shipping" => {
            let (Some(country), Some(currency)) = (country, currency) else {
                let e = "Choose the shop's country and currency first.".to_owned();
                return Ok(welcome_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            };
            let Some(price) = amount(&f.get("price"), &currency) else {
                let e = format!("The price is an amount in {currency}, like 4.95.");
                return Ok(welcome_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            };
            let free_from = match f.get("free_from").as_str() {
                "" => None,
                t => match amount(t, &currency) {
                    Some(a) if a > 0 => Some(a),
                    _ => {
                        let e = "“Free from” is an amount, like 50.00.".to_owned();
                        return Ok(
                            welcome_page(cx, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await
                        );
                    }
                },
            };
            let name = country_name(&country).unwrap_or(&country).to_owned();
            let (status, zone) = cx
                .call(
                    Method::POST,
                    "/api/shipping/zones",
                    Some(&json!({ "name": name, "countries": [country] })),
                )
                .await;
            if status != 201 {
                return Ok(step_failed(cx, status, &zone).await);
            }
            let rates = format!(
                "/api/shipping/zones/{}/rates",
                zone["id"].as_str().unwrap_or_default()
            );
            let mut paid =
                json!({ "name": "Standard", "currency": currency, "price_minor": price });
            let mut calls = Vec::new();
            if let Some(m) = free_from {
                paid["max_subtotal_minor"] = json!(m - 1);
                calls.push((
                    Method::POST,
                    rates.clone(),
                    json!({
                        "name": "Free shipping", "currency": currency, "price_minor": 0,
                        "min_subtotal_minor": m,
                    }),
                ));
            }
            calls.push((Method::POST, rates, paid));
            calls
        }
        _ => return Ok(cx.failed(404, &Value::Null)),
    };
    for (method, path, body) in calls {
        let (status, out) = cx.call(method, &path, Some(&body)).await;
        if !(200..300).contains(&status) {
            return Ok(step_failed(cx, status, &out).await);
        }
    }
    Ok(Redirect::to(&format!("/admin/{shop}/welcome")).into_response())
}

async fn step_failed(cx: Ctx, status: u16, out: &Value) -> Response {
    match status {
        401 => to_login(),
        409 | 422 => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            welcome_page(cx, Some(message(out)), code).await
        }
        _ => cx.failed(status, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_shop_addresses() {
        assert_eq!(slug("Café Ölberg"), "cafe-olberg");
        assert_eq!(slug("  The   Tea Shop! "), "the-tea-shop");
        assert_eq!(slug("42 Socks"), "shop-42-socks");
        assert_eq!(slug("Straße"), "strase");
        assert_eq!(slug("New"), "new-shop");
        assert_eq!(slug(""), "shop");
        assert_eq!(slug(&"a".repeat(80)).len(), 63);
        for name in ["Café Ölberg", "42 Socks", "New", "", "www"] {
            assert!(TenantId::parse(&slug(name)).is_ok(), "{name}");
        }
    }
}
