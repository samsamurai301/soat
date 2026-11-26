//! B1: product options and variants.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn product(client: &TestClient, token: &str, shop: &str) -> String {
    let p: Value = client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": "TEE", "title": "Tee", "price_minor": 2000, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    p["id"].as_str().unwrap().to_owned()
}

async fn set_options(client: &TestClient, token: &str, id: &str, options: Value, status: u16) {
    client
        .put(&format!("/api/products/{id}/options"))
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "options": options }))
        .send()
        .await
        .assert_status(status);
}

async fn add_variant(
    client: &TestClient,
    token: &str,
    id: &str,
    body: Value,
    status: u16,
) -> Value {
    client
        .post(&format!("/api/products/{id}/variants"))
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&body)
        .send()
        .await
        .assert_status(status)
        .json()
}

#[tokio::test]
async fn options_define_variants() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let id = product(&client, &admin, "acme").await;
    set_options(
        &client,
        &admin,
        &id,
        json!([{ "name": "Size", "values": ["S", "M"] }, { "name": "Color", "values": ["Red"] }]),
        200,
    )
    .await;

    let m_red = add_variant(
        &client,
        &admin,
        &id,
        json!({ "sku": "TEE-M-RED", "options": ["M", "Red"] }),
        201,
    )
    .await;
    assert_eq!(m_red["title"], "M / Red");
    assert_eq!(m_red["price_minor"], 2000);
    assert_eq!(m_red["currency"], "EUR");
    assert!(m_red["id"].as_str().unwrap().starts_with("var_"));

    // Wrong arity, unknown value, same values twice, same SKU twice.
    add_variant(
        &client,
        &admin,
        &id,
        json!({ "sku": "X", "options": ["M"] }),
        422,
    )
    .await;
    add_variant(
        &client,
        &admin,
        &id,
        json!({ "sku": "X", "options": ["XL", "Red"] }),
        422,
    )
    .await;
    add_variant(
        &client,
        &admin,
        &id,
        json!({ "sku": "X", "options": ["M", "Red"] }),
        409,
    )
    .await;
    add_variant(
        &client,
        &admin,
        &id,
        json!({ "sku": "TEE-M-RED", "options": ["S", "Red"] }),
        409,
    )
    .await;

    // Adding a value is fine; dropping one a variant uses is not.
    set_options(
        &client,
        &admin,
        &id,
        json!([{ "name": "Size", "values": ["S", "M", "L"] }, { "name": "Color", "values": ["Red", "Blue"] }]),
        200,
    )
    .await;
    set_options(
        &client,
        &admin,
        &id,
        json!([{ "name": "Size", "values": ["S"] }]),
        409,
    )
    .await;
    set_options(
        &client,
        &admin,
        &id,
        json!([{ "name": "Size", "values": ["S", "s"] }]),
        422,
    )
    .await;

    let vid = m_red["id"].as_str().unwrap();
    let changed: Value = client
        .patch(&format!("/api/products/{id}/variants/{vid}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "price_minor": 2500 }))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(changed["price_minor"], 2500);
    assert_eq!(changed["sku"], "TEE-M-RED");

    client
        .delete(&format!("/api/products/{id}/variants/{vid}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    // With no variants left, options can shrink.
    set_options(
        &client,
        &admin,
        &id,
        json!([{ "name": "Size", "values": ["S"] }]),
        200,
    )
    .await;

    let events: Value = client
        .get("/api/events?sort=id")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    let kinds: Vec<&str> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .filter(|k| k.starts_with("variant.") || k.starts_with("product.options"))
        .collect();
    assert_eq!(
        kinds,
        [
            "product.options_changed",
            "variant.created",
            "product.options_changed",
            "variant.updated",
            "variant.deleted",
            "product.options_changed"
        ]
    );
}

#[tokio::test]
async fn variants_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = product(&client, &admin, "globex").await;
    client
        .put(&format!("/api/products/{theirs}/options"))
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .json(&json!({ "options": [{ "name": "Size", "values": ["S"] }] }))
        .send()
        .await
        .assert_status(200);
    let variant: Value = client
        .post(&format!("/api/products/{theirs}/variants"))
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .json(&json!({ "sku": "G-S", "options": ["S"] }))
        .send()
        .await
        .assert_status(201)
        .json();
    let vid = variant["id"].as_str().unwrap();

    // From acme, globex's product and variant do not exist.
    for path in [
        format!("/api/products/{theirs}/options"),
        format!("/api/products/{theirs}/variants"),
    ] {
        client
            .get(&path)
            .header("x-soat-tenant", "acme")
            .bearer(&admin)
            .send()
            .await
            .assert_status(404);
    }
    add_variant(
        &client,
        &admin,
        &theirs,
        json!({ "sku": "X", "options": ["S"] }),
        404,
    )
    .await;
    client
        .patch(&format!("/api/products/{theirs}/variants/{vid}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "price_minor": 1 }))
        .send()
        .await
        .assert_status(404);

    // A variant id under another product of the same shop is a 404 too.
    let ours = product(&client, &admin, "acme").await;
    client
        .delete(&format!("/api/products/{ours}/variants/{vid}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);

    // The same SKU is fine in another shop.
    set_options(
        &client,
        &admin,
        &ours,
        json!([{ "name": "Size", "values": ["S"] }]),
        200,
    )
    .await;
    add_variant(
        &client,
        &admin,
        &ours,
        json!({ "sku": "G-S", "options": ["S"] }),
        201,
    )
    .await;
}
