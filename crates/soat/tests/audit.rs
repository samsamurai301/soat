//! The event log doubles as the audit log: every change, and who made it.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn events(client: &TestClient, token: &str, query: &str) -> Value {
    client
        .get(&format!("/api/events{query}"))
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .send()
        .await
        .assert_status(200)
        .json()
}

async fn me(client: &TestClient, token: &str) -> String {
    let me: Value = client
        .get("/api/auth/me")
        .bearer(token)
        .send()
        .await
        .assert_status(200)
        .json();
    me["id"].as_str().expect("user id").to_owned()
}

#[tokio::test]
async fn changes_are_logged_with_their_actor() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let admin_id = me(&client, &admin).await;

    client
        .put("/api/roles/editor")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "permissions": ["products:read"] }))
        .send()
        .await
        .assert_status(200);
    let key: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "name": "script", "permissions": ["products:write"] }))
        .send()
        .await
        .assert_status(201)
        .json();
    let secret = key["secret"].as_str().unwrap();
    let key_id = key["key"]["id"].as_str().unwrap();
    client
        .post("/api/products")
        .bearer(secret)
        .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);

    let all = events(&client, &admin, "?sort=id").await;
    let kinds: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "tenant.created",
            "role.updated",
            "api_key.created",
            "product.created"
        ]
    );
    for e in all["items"].as_array().unwrap() {
        assert!(e["occurred_at"].as_str().unwrap().ends_with('Z'));
    }

    let by_admin = events(&client, &admin, &format!("?actor={admin_id}")).await;
    assert_eq!(by_admin["total"], 3);
    let by_key = events(&client, &admin, &format!("?actor={key_id}")).await;
    assert_eq!(by_key["total"], 1);
    assert_eq!(by_key["items"][0]["kind"], "product.created");

    // The key's secret never reaches the log.
    assert!(!all.to_string().contains(secret));
}

#[tokio::test]
async fn the_log_needs_events_read() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let key: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "name": "script", "permissions": ["products:read"] }))
        .send()
        .await
        .assert_status(201)
        .json();
    client
        .get("/api/events")
        .bearer(key["secret"].as_str().unwrap())
        .send()
        .await
        .assert_status(403);
}
