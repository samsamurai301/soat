//! API keys: scoped, shop-bound, shown once, revocable.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn create_key(client: &TestClient, token: &str, permissions: &[&str]) -> Value {
    client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "name": "script", "permissions": permissions }))
        .send()
        .await
        .assert_status(201)
        .json()
}

fn product() -> Value {
    json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" })
}

#[tokio::test]
async fn a_key_acts_for_its_shop_within_its_permissions() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let created = create_key(&client, &admin, &["products:read"]).await;
    let secret = created["secret"].as_str().unwrap();
    assert!(secret.starts_with("soat_sk_acme_"));

    // No tenant header needed: the key names its shop.
    client
        .get("/api/products")
        .bearer(secret)
        .send()
        .await
        .assert_status(200);
    // Outside its permissions: 403.
    client
        .post("/api/products")
        .bearer(secret)
        .json(&product())
        .send()
        .await
        .assert_status(403);
    // Pointing it at another shop: 404.
    client
        .get("/api/products")
        .header("x-soat-tenant", "globex")
        .bearer(secret)
        .send()
        .await
        .assert_status(404);
    // A key cannot mint keys.
    client
        .post("/api/api-keys")
        .bearer(secret)
        .json(&json!({ "name": "more", "permissions": [] }))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn forged_or_revoked_keys_are_rejected() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let created = create_key(&client, &admin, &["products:read"]).await;
    let secret = created["secret"].as_str().unwrap();

    for forged in ["soat_sk_acme_nope", "soat_sk_", "soat_sk_Not-A-Shop_x"] {
        client
            .get("/api/products")
            .bearer(forged)
            .send()
            .await
            .assert_status(401);
    }

    let id = created["key"]["id"].as_str().unwrap();
    client
        .delete(&format!("/api/api-keys/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    client
        .get("/api/products")
        .bearer(secret)
        .send()
        .await
        .assert_status(401);

    let keys: Vec<Value> = client
        .get("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["revoked"], true);
    assert!(keys[0].get("secret").is_none());
}

#[tokio::test]
async fn keys_never_exceed_their_creator_or_name_unknown_permissions() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    client
        .put("/api/roles/integrator")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "permissions": ["api_keys:manage", "products:read"] }))
        .send()
        .await
        .assert_status(200);
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "email": "dev@example.com", "password": "dev password 12", "role": "integrator" }))
        .send()
        .await
        .assert_status(201);
    let session: Value = client
        .post("/api/auth/login")
        .json(&json!({ "email": "dev@example.com", "password": "dev password 12" }))
        .send()
        .await
        .json();
    let dev = session["token"].as_str().unwrap();

    create_key(&client, dev, &["products:read"]).await;
    for (perms, status) in [(json!(["products:write"]), 403), (json!(["made:up"]), 422)] {
        client
            .post("/api/api-keys")
            .header("x-soat-tenant", "acme")
            .bearer(dev)
            .json(&json!({ "name": "x", "permissions": perms }))
            .send()
            .await
            .assert_status(status);
    }
}
