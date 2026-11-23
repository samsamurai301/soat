//! Customer account pages: sign in, register, sign out, the profile and
//! past orders. The session lives in an HttpOnly cookie.

use serde_json::{json, Value};
use soat_customers::accounts::{self, Login, MeChange, Register, SignedIn, SESSION_DAYS};
use soat_tenancy::Shop;
use zapline::db::{Db, ListParams};
use zapline::prelude::*;

use super::{
    cart_cookie, carts, cookie, money, offset, order_status, order_values, put_cookie, upper_first,
    CartToken, View,
};

/// The cookie holding the customer's session token.
pub const CUSTOMER_COOKIE: &str = "soat_customer";

const ORDERS_PER_PAGE: i64 = 20;

/// Starts the session in a cookie and gives the buyer's cart to the
/// customer.
async fn signed_in(
    db: &Db,
    shop: &Shop,
    headers: &HeaderMap,
    secure: bool,
    session: accounts::CustomerSession,
) -> Result<Response> {
    if let Some((id, token)) = cart_cookie(headers) {
        let holder = CartToken::new(shop.clone(), &token);
        if let Ok((mut tx, id)) = holder.open(db, &id).await {
            carts::attach(&mut tx, shop, &id, &session.customer.id).await?;
            tx.commit().await?;
        }
    }
    let mut res = Redirect::to("/account").into_response();
    put_cookie(
        &mut res,
        CUSTOMER_COOKIE,
        &session.token,
        SESSION_DAYS * 86_400,
        secure,
    );
    Ok(res)
}

fn first_error(e: &zapline::validate::ValidationErrors) -> String {
    zapline::validate::field_errors(e)
        .first()
        .map(|f| match f.field.as_str() {
            "password" => "the password needs at least 10 characters".to_owned(),
            "email" => "that is not an email address".to_owned(),
            other => format!("{other}: {}", f.message),
        })
        .unwrap_or_else(|| "check the form".to_owned())
}

/// The sign-in form.
#[get("/account/login", tag = "storefront")]
pub(crate) async fn login_page(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    if v.customer.is_some() {
        v.tx.commit().await?;
        return Ok(Redirect::to("/account").into_response());
    }
    v.set("page_title", json!("Sign in"));
    v.render("login", StatusCode::OK).await
}

/// The sign-in form's fields.
#[model]
pub struct LoginForm {
    pub email: String,
    pub password: String,
}

/// Sign in (a form post); a wrong email or password shows the form again.
#[post("/account/login", tag = "storefront")]
pub(crate) async fn login(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<LoginForm>,
) -> Result<Response> {
    let l = Login {
        email: f.email.trim().to_owned(),
        password: f.password,
    };
    let session = match l.validate() {
        Ok(()) => accounts::sign_in(&db, &shop, l).await,
        Err(_) => Err(Error::unauthorized()),
    };
    match session {
        Ok(s) => signed_in(&db, &shop, &headers, conn.secure, s).await,
        Err(e) if e.status() == StatusCode::UNAUTHORIZED => {
            let mut v = View::open(&db, &shop, &headers).await?;
            v.set("page_title", json!("Sign in"));
            v.set("email", json!(f.email));
            v.set("error", json!("Wrong email or password."));
            v.render("login", StatusCode::UNAUTHORIZED).await
        }
        Err(e) => Err(e),
    }
}

/// The registration form.
#[get("/account/register", tag = "storefront")]
pub(crate) async fn register_page(db: Db, shop: Shop, headers: HeaderMap) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    if v.customer.is_some() {
        v.tx.commit().await?;
        return Ok(Redirect::to("/account").into_response());
    }
    v.set("page_title", json!("Create an account"));
    v.render("register", StatusCode::OK).await
}

/// The registration form's fields.
#[model]
pub struct RegisterForm {
    pub email: String,
    pub password: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Present when the box is ticked.
    pub accepts_marketing: Option<String>,
}

/// Create an account (a form post) and sign in. Problems show the form
/// again, as typed (but for the password).
#[post("/account/register", tag = "storefront")]
pub(crate) async fn register(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
    Form(f): Form<RegisterForm>,
) -> Result<Response> {
    let trim = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let r = Register {
        email: f.email.trim().to_owned(),
        password: f.password.clone(),
        first_name: trim(&f.first_name),
        last_name: trim(&f.last_name),
        accepts_marketing: f.accepts_marketing.is_some(),
    };
    let outcome = match r.validate() {
        Ok(()) => accounts::create_account(&db, &shop, r).await,
        Err(e) => Err(Error::unprocessable(first_error(&e)).with_code("validation_error")),
    };
    let e = match outcome {
        Ok(s) => return signed_in(&db, &shop, &headers, conn.secure, s).await,
        Err(e) => e,
    };
    let msg = match e.code() {
        Some("email_taken") => "An account with this email exists. Sign in instead.".to_owned(),
        Some("activation_required") => {
            "This email has ordered here before. Activating it by email is coming soon.".to_owned()
        }
        Some("validation_error") => e.message().to_owned(),
        _ => return Err(e),
    };
    let mut v = View::open(&db, &shop, &headers).await?;
    v.set("page_title", json!("Create an account"));
    v.set("email", json!(f.email));
    v.set("first_name", json!(f.first_name));
    v.set("last_name", json!(f.last_name));
    v.set("error", json!(upper_first(&msg)));
    v.render("register", StatusCode::UNPROCESSABLE_ENTITY).await
}

/// Sign out (a form post): ends the session and drops its cookie.
#[post("/account/logout", tag = "storefront")]
pub(crate) async fn logout(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    conn: ConnInfo,
) -> Result<Response> {
    if let Some(token) = cookie(&headers, CUSTOMER_COOKIE) {
        // An expired session is signed out already.
        let _ = SignedIn::new(shop, &token).sign_out(&db).await;
    }
    let mut res = Redirect::to("/").into_response();
    put_cookie(&mut res, CUSTOMER_COOKIE, "", 0, conn.secure);
    Ok(res)
}

/// The customer's account: orders (20 a page, `?offset=`) and details.
#[get("/account", tag = "storefront")]
pub(crate) async fn account_page(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    params: ListParams,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to("/account/login").into_response());
    };
    let at = offset(&params);
    let query = format!("limit={ORDERS_PER_PAGE}&offset={at}");
    let page =
        soat_orders::customer_orders(&mut v.tx, &customer, &ListParams::from_query(&query)).await?;
    let orders: Vec<Value> = page
        .items
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "number": o.number,
                "placed": o.placed_at.get(..10).unwrap_or_default(),
                "status": order_status(&o.status, &o.financial_status, &o.fulfillment_status),
                "total": money(o.total_minor, &o.currency),
            })
        })
        .collect();
    if at > 0 {
        let prev = (at - ORDERS_PER_PAGE).max(0);
        v.set("prev_url", json!(format!("/account?offset={prev}")));
    }
    if at + ORDERS_PER_PAGE < page.total {
        v.set(
            "next_url",
            json!(format!("/account?offset={}", at + ORDERS_PER_PAGE)),
        );
    }
    v.set("page_title", json!("Your account"));
    v.set("orders", Value::Array(orders));
    v.set("saved", json!(params.get("saved").is_some()));
    v.render("account", StatusCode::OK).await
}

/// The account form's fields.
#[model]
pub struct AccountForm {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
    /// Present when the box is ticked.
    pub accepts_marketing: Option<String>,
}

/// Change your details (a form post). Publishes `customer.updated`.
#[post("/account", tag = "storefront")]
pub(crate) async fn update_account(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Form(f): Form<AccountForm>,
) -> Result<Response> {
    let Some(token) = cookie(&headers, CUSTOMER_COOKIE) else {
        return Ok(Redirect::to("/account/login").into_response());
    };
    let session = SignedIn::new(shop.clone(), &token);
    let Ok((mut tx, customer)) = session.begin(&db).await else {
        return Ok(Redirect::to("/account/login").into_response());
    };
    let trim = |v: Option<String>| v.map(|v| v.trim().to_owned());
    let c = MeChange {
        first_name: trim(f.first_name),
        last_name: trim(f.last_name),
        phone: trim(f.phone),
        accepts_marketing: Some(f.accepts_marketing.is_some()),
    };
    c.validate().map_err(zapline::validate::validation_error)?;
    accounts::change_profile(&mut tx, &shop, &customer, &c).await?;
    tx.commit().await?;
    Ok(Redirect::to("/account?saved=1").into_response())
}

/// One of the customer's orders; anyone else's is not found.
#[get("/account/orders/{id}", tag = "storefront")]
pub(crate) async fn account_order(
    db: Db,
    shop: Shop,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        v.tx.commit().await?;
        return Ok(Redirect::to("/account/login").into_response());
    };
    let Some(order) = soat_orders::customer_order(&mut v.tx, &shop, &customer, &id).await? else {
        return v.not_found().await;
    };
    let payments = soat_payments::for_reference(&mut v.tx, &shop, &order.id).await?;
    order_values(&mut v, &order, &payments);
    v.set("account", json!(true));
    v.render("order", StatusCode::OK).await
}
