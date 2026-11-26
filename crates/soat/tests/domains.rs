//! Shop resolution: by API key, by header, or by the request's domain.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn add(client: &TestClient, token: &str, shop: &str, domain: &str, status: u16) {
    client
        .post("/api/domains")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "domain": domain }))
        .send()
        .await
        .assert_status(status);
}

async fn product(client: &TestClient, token: &str, shop: &str, sku: &str) {
    client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": sku, "title": sku, "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);
}

async fn skus_on(client: &TestClient, token: &str, host: &str) -> Vec<String> {
    let page: Value = client
        .get("/api/products")
        .header("host", host)
        .bearer(token)
        .send()
        .await
        .assert_status(200)
        .json();
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["sku"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_host_picks_the_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add(&client, &admin, "acme", "Shop.Acme.example.", 201).await;
    add(&client, &admin, "globex", "globex.example", 201).await;
    product(&client, &admin, "acme", "A").await;
    product(&client, &admin, "globex", "G").await;

    assert_eq!(skus_on(&client, &admin, "shop.acme.example").await, ["A"]);
    assert_eq!(skus_on(&client, &admin, "globex.example:8080").await, ["G"]);

    // An unknown host is the same as no shop at all.
    client
        .get("/api/products")
        .header("host", "nobody.example")
        .bearer(&admin)
        .send()
        .await
        .assert_status(400);
    // The header wins over the host.
    let page: Value = client
        .get("/api/products")
        .header("host", "globex.example")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(page["items"][0]["sku"], "A");
}

#[tokio::test]
async fn a_domain_belongs_to_one_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add(&client, &admin, "acme", "acme.example", 201).await;
    add(&client, &admin, "globex", "ACME.example", 409).await;
    add(&client, &admin, "globex", "localhost", 422).await;
    add(&client, &admin, "globex", "10.0.0.1", 422).await;

    // Another shop cannot see or remove it.
    let theirs: Value = client
        .get("/api/domains")
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(theirs, json!([]));
    client
        .delete("/api/domains/acme.example")
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);

    client
        .delete("/api/domains/acme.example")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    add(&client, &admin, "globex", "acme.example", 201).await;
}

#[tokio::test]
async fn a_host_does_not_let_strangers_in() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    add(&client, &admin, "acme", "acme.example", 201).await;
    client
        .post("/api/tenants")
        .bearer(&admin)
        .json(&json!({ "id": "other", "name": "Other" }))
        .send()
        .await
        .assert_status(201);
    // Make an owner of `other` who is not in `acme`.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "other")
        .bearer(&admin)
        .json(&json!({ "email": "stranger@example.com", "password": "stranger password", "role": "owner" }))
        .send()
        .await
        .assert_status(201);
    let login: Value = client
        .post("/api/auth/login")
        .json(&json!({ "email": "stranger@example.com", "password": "stranger password" }))
        .send()
        .await
        .assert_status(200)
        .json();
    client
        .get("/api/products")
        .header("host", "acme.example")
        .bearer(login["token"].as_str().unwrap())
        .send()
        .await
        .assert_status(404);
}
