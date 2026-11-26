//! B2: hand-picked collections.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn product(client: &TestClient, token: &str, shop: &str, sku: &str) -> String {
    let p: Value = client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": sku, "title": sku, "price_minor": 100, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    p["id"].as_str().unwrap().to_owned()
}

async fn collection(client: &TestClient, token: &str, shop: &str, body: Value) -> Value {
    client
        .post("/api/collections")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&body)
        .send()
        .await
        .assert_status(201)
        .json()
}

async fn put(client: &TestClient, token: &str, shop: &str, path: &str, status: u16) {
    client
        .put(path)
        .header("x-soat-tenant", shop)
        .bearer(token)
        .send()
        .await
        .assert_status(status);
}

async fn skus(client: &TestClient, token: &str, col: &str) -> Vec<String> {
    let page: Value = client
        .get(&format!("/api/collections/{col}/products"))
        .header("x-soat-tenant", "acme")
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
async fn collections_hold_products_in_order() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let c = collection(&client, &admin, "acme", json!({ "title": "Summer Sale!" })).await;
    assert_eq!(c["handle"], "summer-sale");
    let col = c["id"].as_str().unwrap();
    assert!(col.starts_with("col_"));

    let a = product(&client, &admin, "acme", "A").await;
    let b = product(&client, &admin, "acme", "B").await;
    let z = product(&client, &admin, "acme", "Z").await;
    for p in [&b, &a, &z, &a] {
        put(
            &client,
            &admin,
            "acme",
            &format!("/api/collections/{col}/products/{p}"),
            204,
        )
        .await;
    }
    assert_eq!(skus(&client, &admin, col).await, ["B", "A", "Z"]);

    client
        .put(&format!("/api/collections/{col}/order"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "product_ids": [z, a, b] }))
        .send()
        .await
        .assert_status(204);
    assert_eq!(skus(&client, &admin, col).await, ["Z", "A", "B"]);
    client
        .put(&format!("/api/collections/{col}/order"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "product_ids": [z, a] }))
        .send()
        .await
        .assert_status(422);

    client
        .delete(&format!("/api/collections/{col}/products/{a}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    // Deleting a product takes it out of its collections.
    client
        .delete(&format!("/api/products/{z}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    assert_eq!(skus(&client, &admin, col).await, ["B"]);

    // Handles are unique per shop and well formed.
    client
        .post("/api/collections")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "title": "Again", "handle": "summer-sale" }))
        .send()
        .await
        .assert_status(409);
    client
        .patch(&format!("/api/collections/{col}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "handle": "Not A Handle" }))
        .send()
        .await
        .assert_status(422);
    let renamed: Value = client
        .patch(&format!("/api/collections/{col}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "title": "Summer" }))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(renamed["handle"], "summer-sale");
    assert_eq!(renamed["title"], "Summer");
}

#[tokio::test]
async fn collections_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = collection(&client, &admin, "globex", json!({ "title": "Theirs" })).await;
    let theirs = theirs["id"].as_str().unwrap();
    let their_product = product(&client, &admin, "globex", "G").await;
    let ours = collection(&client, &admin, "acme", json!({ "title": "Theirs" })).await;
    let ours = ours["id"].as_str().unwrap();

    for path in [
        format!("/api/collections/{theirs}"),
        format!("/api/collections/{theirs}/products"),
    ] {
        client
            .get(&path)
            .header("x-soat-tenant", "acme")
            .bearer(&admin)
            .send()
            .await
            .assert_status(404);
    }
    // Another shop's product cannot go into our collection.
    put(
        &client,
        &admin,
        "acme",
        &format!("/api/collections/{ours}/products/{their_product}"),
        404,
    )
    .await;
    client
        .delete(&format!("/api/collections/{theirs}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);
    let list: Value = client
        .get("/api/collections")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(list["total"], 1);
}
