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

async fn list(client: &TestClient, token: &str, tenant: &str, query: &str) -> Page {
    client
        .get(&format!("/api/products{query}"))
        .header("x-soat-tenant", tenant)
        .bearer(token)
        .send()
        .await
        .assert_status(200)
        .json()
}

fn shoe() -> serde_json::Value {
    json!({ "sku": "SHOE-1", "title": "Trail shoe", "price_minor": 12999, "currency": "EUR" })
}

#[tokio::test]
async fn creates_and_lists_products_for_a_tenant() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();
    assert_eq!(created.price_minor, 12999);

    assert!(created.id.starts_with("prod_"));

    let page = list(&client, &token, "acme", "").await;
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].sku, "SHOE-1");
}

#[tokio::test]
async fn tenants_cannot_see_each_others_products() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();

    let page = list(&client, &token, "globex", "").await;
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
    client
        .get(&format!("/api/products/{}", created.id))
        .header("x-soat-tenant", "globex")
        .bearer(&token)
        .send()
        .await
        .assert_status(404);

    // The same SKU is fine in another tenant.
    client
        .post("/api/products")
        .header("x-soat-tenant", "globex")
        .bearer(&token)
        .json(&shoe())
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn duplicate_sku_in_one_tenant_is_a_conflict() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    for expected in [201, 409] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .bearer(&token)
            .json(&shoe())
            .send()
            .await
            .assert_status(expected);
    }
}

#[tokio::test]
async fn missing_unknown_or_malformed_tenant_is_rejected() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    client.get("/api/products").send().await.assert_status(400);
    client
        .get("/api/products")
        .header("x-soat-tenant", "Not A Slug")
        .bearer(&token)
        .send()
        .await
        .assert_status(400);
    client
        .get("/api/products")
        .header("x-soat-tenant", "nobody")
        .bearer(&token)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn invalid_products_get_a_422() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    for body in [
        json!({ "sku": "", "title": "x", "price_minor": 1, "currency": "EUR" }),
        json!({ "sku": "A", "title": "x", "price_minor": -1, "currency": "EUR" }),
        json!({ "sku": "A", "title": "x", "price_minor": 1, "currency": "eur" }),
    ] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .bearer(&token)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
}

#[tokio::test]
async fn openapi_documents_the_catalog() {
    let (client, _) = common::with_tenants(&[]).await;
    client
        .get("/openapi.json")
        .send()
        .await
        .assert_status(200)
        .assert_contains("/api/products");
}

#[tokio::test]
async fn lists_page_search_and_sort_within_the_tenant() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    for (sku, title, price) in [
        ("A", "Trail shoe", 300),
        ("B", "Road shoe", 100),
        ("C", "Wool sock", 200),
    ] {
        client
            .post("/api/products")
            .header("x-soat-tenant", "acme")
            .bearer(&token)
            .json(&json!({ "sku": sku, "title": title, "price_minor": price, "currency": "EUR" }))
            .send()
            .await
            .assert_status(201);
    }
    // Another tenant's product never counts.
    client
        .post("/api/products")
        .header("x-soat-tenant", "globex")
        .bearer(&token)
        .json(&json!({ "sku": "Z", "title": "Trail shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);

    let first = list(&client, &token, "acme", "?limit=2").await;
    assert_eq!(first.total, 3);
    assert_eq!(first.items.len(), 2);
    let second = list(&client, &token, "acme", "?limit=2&offset=2").await;
    assert_eq!(second.items.len(), 1);

    let shoes = list(&client, &token, "acme", "?q=shoe").await;
    assert_eq!(shoes.total, 2);

    let cheapest_first = list(&client, &token, "acme", "?sort=price_minor").await;
    let skus: Vec<_> = cheapest_first
        .items
        .iter()
        .map(|p| p.sku.as_str())
        .collect();
    assert_eq!(skus, ["B", "C", "A"]);

    client
        .get("/api/products?sort=tenant_id")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(400);
}

#[tokio::test]
async fn malformed_or_wrong_kind_ids_are_not_found() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    for id in ["42", "prod_nope", "ordr_01jdx3k9q8m2v4t6w7y0z1a2b3"] {
        client
            .get(&format!("/api/products/{id}"))
            .header("x-soat-tenant", "acme")
            .bearer(&token)
            .send()
            .await
            .assert_status(404);
    }
}

#[tokio::test]
async fn creating_a_product_publishes_an_event() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();
    let events: serde_json::Value = client
        .get("/api/events?kind=product.created")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(events["total"], 1);
    assert_eq!(events["items"][0]["payload"]["id"], created.id.as_str());
}

#[tokio::test]
async fn products_can_be_changed_and_deleted_only_in_their_shop() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    let created: Product = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&shoe())
        .send()
        .await
        .assert_status(201)
        .json();
    let path = format!("/api/products/{}", created.id);
    for shop in ["globex", "acme"] {
        let status = if shop == "acme" { 200 } else { 404 };
        client
            .patch(&path)
            .header("x-soat-tenant", shop)
            .bearer(&token)
            .json(&serde_json::json!({ "title": "Boot" }))
            .send()
            .await
            .assert_status(status);
    }
    let got: Product = client
        .get(&path)
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(got.title, "Boot");
    assert_eq!(got.price_minor, created.price_minor);

    client
        .delete(&path)
        .header("x-soat-tenant", "globex")
        .bearer(&token)
        .send()
        .await
        .assert_status(404);
    client
        .delete(&path)
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(204);
    client
        .get(&path)
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(404);
}
