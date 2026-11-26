//! C2: customer accounts on the storefront.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn public(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match token {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 || status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

fn signup(email: &str) -> Value {
    json!({ "email": email, "password": "a long password", "first_name": "Ada" })
}

#[tokio::test]
async fn customers_register_sign_in_and_manage_their_profile() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let s = public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("Ada@example.com"),
        201,
    )
    .await;
    let token = s["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("soat_ct_"));
    assert_eq!(s["customer"]["email"], "ada@example.com");
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("ada@example.com"),
        409,
    )
    .await;
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": "b@example.com", "password": "short" }),
        422,
    )
    .await;

    let me = public(
        &client,
        "acme",
        "GET",
        "/api/storefront/account",
        Some(&token),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(me["first_name"], "Ada");
    assert!(me.get("note").is_none());
    let me = public(
        &client,
        "acme",
        "PATCH",
        "/api/storefront/account",
        Some(&token),
        json!({ "last_name": "Lovelace", "accepts_marketing": true }),
        200,
    )
    .await;
    assert_eq!(me["last_name"], "Lovelace");

    // Wrong password and unknown email look the same.
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/login",
        None,
        json!({ "email": "ada@example.com", "password": "wrong password" }),
        401,
    )
    .await;
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/login",
        None,
        json!({ "email": "nobody@example.com", "password": "wrong password" }),
        401,
    )
    .await;
    let again = public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/login",
        None,
        json!({ "email": "ada@example.com", "password": "a long password" }),
        200,
    )
    .await;
    let second = again["token"].as_str().unwrap();

    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/logout",
        Some(&token),
        Value::Null,
        204,
    )
    .await;
    public(
        &client,
        "acme",
        "GET",
        "/api/storefront/account",
        Some(&token),
        Value::Null,
        401,
    )
    .await;
    public(
        &client,
        "acme",
        "GET",
        "/api/storefront/account",
        Some(second),
        Value::Null,
        200,
    )
    .await;

    // Staff see that the customer has an account, and the consent time.
    let page: Value = client
        .get("/api/customers")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(page["items"][0]["has_account"], true);
    assert!(page["items"][0]["marketing_consent_at"].is_string());

    // A staff session is not a customer session, nor the other way round.
    public(
        &client,
        "acme",
        "GET",
        "/api/storefront/account",
        Some(&admin),
        Value::Null,
        401,
    )
    .await;
    client
        .get("/api/customers")
        .header("x-soat-tenant", "acme")
        .bearer(second)
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn a_known_email_cannot_be_claimed_by_registering() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    client
        .post("/api/customers")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "email": "guest@example.com" }))
        .send()
        .await
        .assert_status(201);
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("guest@example.com"),
        409,
    )
    .await;
    public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/login",
        None,
        json!({ "email": "guest@example.com", "password": "anything at all" }),
        401,
    )
    .await;
}

#[tokio::test]
async fn accounts_belong_to_one_shop() {
    let (client, _) = common::with_tenants(&["acme", "globex"]).await;
    let s = public(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("ada@example.com"),
        201,
    )
    .await;
    let token = s["token"].as_str().unwrap();
    // Not valid in another shop; the same email is a separate account there.
    public(
        &client,
        "globex",
        "GET",
        "/api/storefront/account",
        Some(token),
        Value::Null,
        401,
    )
    .await;
    public(
        &client,
        "globex",
        "POST",
        "/api/storefront/account/login",
        None,
        json!({ "email": "ada@example.com", "password": "a long password" }),
        401,
    )
    .await;
    public(
        &client,
        "globex",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("ada@example.com"),
        201,
    )
    .await;
    public(
        &client,
        "nowhere",
        "POST",
        "/api/storefront/account/register",
        None,
        signup("ada@example.com"),
        404,
    )
    .await;
}
