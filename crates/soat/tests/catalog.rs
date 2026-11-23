//! End-to-end tests against a fresh Postgres schema per test.

mod common;

use serde_json::json;
use zapline::test::TestClient;

use soat_catalog::Product;

/// One page of a list response.
#[derive(serde::Deserialize)]
struct Page {
    items: Vec<Product>,
    total: i64,
}

async fn list(client: &TestClient, tenant: &str, query: &str) -> Page {
    client
        .get(&format!("/api/products{query}"))
        .header("x-soat-tenant", tenant)
        .send()
        .await
        .assert_status(200)
        .json()
}

async fn with_tenants(ids: &[&str]) -> TestClient {
    let client = common::client().await;
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

    assert!(created.id.starts_with("prod_"));

    let page = list(&client, "acme", "").await;
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].sku, "SHOE-1");
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

    let page = list(&client, "globex", "").await;
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
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

#[tokio::test]
async fn lists_page_search_and_sort_within_the_tenant() {
    let client = with_tenants(&["acme", "globex"]).await;
    for (sku, title, price) in [
        ("A", "Trail shoe", 300),
        ("B", "Road shoe", 100),
        ("C", "Wool sock", 200),
    ] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .json(&json!({ "sku": sku, "title": title, "price_minor": price, "currency": "EUR" }))
            .send()
            .await
            .assert_status(201);
    }
    // Another tenant's product never counts.
    client
        .post("/api/products")
        .header("x-soat-tenant", "globex")
        .json(&json!({ "sku": "Z", "title": "Trail shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);

    let first = list(&client, "acme", "?limit=2").await;
    assert_eq!(first.total, 3);
    assert_eq!(first.items.len(), 2);
    let second = list(&client, "acme", "?limit=2&offset=2").await;
    assert_eq!(second.items.len(), 1);

    let shoes = list(&client, "acme", "?q=shoe").await;
    assert_eq!(shoes.total, 2);

    let cheapest_first = list(&client, "acme", "?sort=price_minor").await;
    let skus: Vec<_> = cheapest_first
        .items
        .iter()
        .map(|p| p.sku.as_str())
        .collect();
    assert_eq!(skus, ["B", "C", "A"]);

    client
        .get("/api/products?sort=tenant_id")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(400);
}

#[tokio::test]
async fn malformed_or_wrong_kind_ids_are_not_found() {
    let client = with_tenants(&["acme"]).await;
    for id in ["42", "prod_nope", "ordr_01jdx3k9q8m2v4t6w7y0z1a2b3"] {
        client
            .get(&format!("/api/products/{id}"))
            .header("x-soat-tenant", "acme")
            .send()
            .await
            .assert_status(404);
    }
}
