//! Setup, login, sessions, shop creation and staff access.

mod common;

use common::{ADMIN_EMAIL, ADMIN_PASSWORD, SETUP_TOKEN};
use serde_json::{json, Value};
use zapline::test::TestClient;

async fn login(client: &TestClient, email: &str, password: &str) -> String {
    let session: Value = client
        .post("/api/auth/login")
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .assert_status(200)
        .json();
    session["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn setup_needs_the_token_and_runs_once() {
    let (client, _) = common::client_and_url().await;
    client
        .post("/api/setup")
        .json(&json!({ "setup_token": "wrong", "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
        .send()
        .await
        .assert_status(403);
    common::setup_admin(&client).await;
    client
        .post("/api/setup")
        .json(&json!({ "setup_token": SETUP_TOKEN, "email": "two@example.com", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .assert_status(409);
}

#[tokio::test]
async fn login_me_and_logout() {
    let (client, _) = common::client_and_url().await;
    common::setup_admin(&client).await;

    // Email is case-insensitive; a wrong password and an unknown email are the same 401.
    let token = login(&client, "Admin@Example.com", ADMIN_PASSWORD).await;
    for (email, password) in [
        (ADMIN_EMAIL, "not the password"),
        ("nobody@example.com", ADMIN_PASSWORD),
    ] {
        client
            .post("/api/auth/login")
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .assert_status(401);
    }

    let me: Value = client
        .get("/api/auth/me")
        .bearer(&token)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(me["email"], ADMIN_EMAIL);
    assert_eq!(me["is_platform_admin"], true);

    client
        .post("/api/auth/logout")
        .bearer(&token)
        .send()
        .await
        .assert_status(204);
    client
        .get("/api/auth/me")
        .bearer(&token)
        .send()
        .await
        .assert_status(401);
    client.get("/api/auth/me").send().await.assert_status(401);
}

#[tokio::test]
async fn only_platform_admins_create_shops() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    client
        .post("/api/tenants")
        .json(&json!({ "id": "anon", "name": "Anon" }))
        .send()
        .await
        .assert_status(401);

    add_staff(&client, &admin, "acme", "clerk@example.com", None).await;
    let clerk = login(&client, "clerk@example.com", "clerk password 123").await;
    client
        .post("/api/tenants")
        .bearer(&clerk)
        .json(&json!({ "id": "mine", "name": "Mine" }))
        .send()
        .await
        .assert_status(403);
}

async fn add_staff(client: &TestClient, token: &str, shop: &str, email: &str, role: Option<&str>) {
    client
        .post("/api/staff")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "email": email, "password": "clerk password 123", "role": role }))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn staff_only_reach_their_own_shops() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add_staff(&client, &admin, "acme", "clerk@example.com", None).await;
    let clerk = login(&client, "clerk@example.com", "clerk password 123").await;

    client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&clerk)
        .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);

    // Not a member of globex: indistinguishable from a shop that does not exist.
    for shop in ["globex", "nowhere"] {
        client
            .get("/api/products")
            .header("x-soat-tenant", shop)
            .bearer(&clerk)
            .send()
            .await
            .assert_status(404);
    }

    // Staff are not owners: they cannot add staff.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&clerk)
        .json(&json!({ "email": "friend@example.com", "password": "friend password 1" }))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn owners_manage_staff() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add_staff(&client, &admin, "acme", "boss@example.com", Some("owner")).await;
    let boss = login(&client, "boss@example.com", "clerk password 123").await;
    add_staff(&client, &boss, "acme", "clerk@example.com", None).await;

    // The same person can join a second shop; their password stays theirs.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .json(&json!({ "email": "clerk@example.com" }))
        .send()
        .await
        .assert_status(201);
    // Adding them twice is a 409; a new email without a password is a 422.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&boss)
        .json(&json!({ "email": "clerk@example.com" }))
        .send()
        .await
        .assert_status(409);
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&boss)
        .json(&json!({ "email": "new@example.com" }))
        .send()
        .await
        .assert_status(422);

    let staff: Vec<Value> = client
        .get("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&boss)
        .send()
        .await
        .assert_status(200)
        .json();
    let emails: Vec<_> = staff.iter().map(|m| m["email"].as_str().unwrap()).collect();
    assert_eq!(
        emails,
        [ADMIN_EMAIL, "boss@example.com", "clerk@example.com"]
    );
}
