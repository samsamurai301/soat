//! End-to-end tests against a fresh in-memory database per test.

use serde_json::json;
use zapline::db::Database;
use zapline::test::TestClient;

use soat::app;
use soat::routes::catalog::Product;

async fn with_tenants(ids: &[&str]) -> TestClient {
    let client = app(Database::sqlite_memory()).test();
    for id in ids {
        client
            .post("/api/tenants")
            .json(&json!({ "id": id, "name": id }))
            .send()
            .await
            .assert_status(201);
    }
    client
}

fn shoe() -> serde_json::Value {
    json!({ "sku": "SHOE-1", "title": "Trail shoe", "price_minor": 12999, "currency": "EUR" })
}

#[tokio::test]
async fn creates_and_lists_products_for_a_tenant() {
    let client = with_tenants(&["acme"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();
    assert_eq!(created.price_minor, 12999);

    let list: Vec<Product> = client
        .get("/api/products")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].sku, "SHOE-1");
}

#[tokio::test]
async fn tenants_cannot_see_each_others_products() {
    let client = with_tenants(&["acme", "globex"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();

    let list: Vec<Product> = client
        .get("/api/products")
        .header("x-soat-tenant", "globex")
        .send()
        .await
        .assert_status(200)
        .json();
    assert!(list.is_empty());
    client
        .get(&format!("/api/products/{}", created.id))
        .header("x-soat-tenant", "globex")
        .send()
        .await
        .assert_status(404);

    // The same SKU is fine in another tenant.
    client
        .post("/api/products")
        .header("x-soat-tenant", "globex")
        .json(&shoe())
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn duplicate_sku_in_one_tenant_is_a_conflict() {
    let client = with_tenants(&["acme"]).await;
    for expected in [201, 409] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .json(&shoe())
            .send()
            .await
            .assert_status(expected);
    }
}

#[tokio::test]
async fn missing_unknown_or_malformed_tenant_is_rejected() {
    let client = with_tenants(&["acme"]).await;
    client.get("/api/products").send().await.assert_status(400);
    client
        .get("/api/products")
        .header("x-soat-tenant", "Not A Slug")
        .send()
        .await
        .assert_status(400);
    client
        .get("/api/products")
        .header("x-soat-tenant", "nobody")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn invalid_products_get_a_422() {
    let client = with_tenants(&["acme"]).await;
    for body in [
        json!({ "sku": "", "title": "x", "price_minor": 1, "currency": "EUR" }),
        json!({ "sku": "A", "title": "x", "price_minor": -1, "currency": "EUR" }),
        json!({ "sku": "A", "title": "x", "price_minor": 1, "currency": "eur" }),
    ] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
}

#[tokio::test]
async fn openapi_documents_the_catalog() {
    let client = with_tenants(&[]).await;
    client
        .get("/openapi.json")
        .send()
        .await
        .assert_status(200)
        .assert_contains("/api/products");
}
