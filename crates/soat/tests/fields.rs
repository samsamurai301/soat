//! B5: custom fields.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn define(client: &TestClient, token: &str, shop: &str, key: &str, body: Value, status: u16) {
    client
        .put(&format!("/api/fields/product/{key}"))
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&body)
        .send()
        .await
        .assert_status(status);
}

async fn product(client: &TestClient, token: &str, shop: &str) -> String {
    let p: Value = client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    p["id"].as_str().unwrap().to_owned()
}

async fn set(
    client: &TestClient,
    token: &str,
    shop: &str,
    id: &str,
    body: Value,
    status: u16,
) -> Value {
    let res = client
        .patch(&format!("/api/products/{id}/fields"))
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&body)
        .send()
        .await;
    res.assert_status(status).json()
}

#[tokio::test]
async fn products_carry_typed_custom_fields() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    define(
        &client,
        &admin,
        "acme",
        "care",
        json!({ "name": "Care", "kind": "text" }),
        200,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "release",
        json!({ "name": "Release", "kind": "date" }),
        200,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "fit",
        json!({ "name": "Fit", "kind": "choice", "choices": ["slim", "regular"] }),
        200,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "Bad-Key",
        json!({ "name": "x", "kind": "text" }),
        422,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "x",
        json!({ "name": "x", "kind": "float" }),
        422,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "x",
        json!({ "name": "x", "kind": "text", "choices": ["a"] }),
        422,
    )
    .await;

    let id = product(&client, &admin, "acme").await;
    let values = set(
        &client,
        &admin,
        "acme",
        &id,
        json!({ "care": "Hand wash", "release": "2027-01-04", "fit": "slim" }),
        200,
    )
    .await;
    assert_eq!(
        values,
        json!({ "care": "Hand wash", "fit": "slim", "release": "2027-01-04" })
    );

    set(
        &client,
        &admin,
        "acme",
        &id,
        json!({ "release": "2027-02-30" }),
        422,
    )
    .await;
    set(&client, &admin, "acme", &id, json!({ "fit": "baggy" }), 422).await;
    set(
        &client,
        &admin,
        "acme",
        &id,
        json!({ "colour": "red" }),
        422,
    )
    .await;
    let values = set(&client, &admin, "acme", &id, json!({ "care": null }), 200).await;
    assert_eq!(values, json!({ "fit": "slim", "release": "2027-01-04" }));

    // A kind change or dropped choice that existing values would break.
    define(
        &client,
        &admin,
        "acme",
        "release",
        json!({ "name": "R", "kind": "integer" }),
        409,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "fit",
        json!({ "name": "Fit", "kind": "choice", "choices": ["regular"] }),
        409,
    )
    .await;
    define(
        &client,
        &admin,
        "acme",
        "fit",
        json!({ "name": "Fit", "kind": "choice", "choices": ["slim", "regular", "loose"] }),
        200,
    )
    .await;

    // Deleting a definition clears its values.
    client
        .delete("/api/fields/product/fit")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    let values: Value = client
        .get(&format!("/api/products/{id}/fields"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(values, json!({ "release": "2027-01-04" }));
    let defs: Value = client
        .get("/api/fields/product")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(defs.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn fields_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    define(
        &client,
        &admin,
        "globex",
        "care",
        json!({ "name": "Care", "kind": "text" }),
        200,
    )
    .await;
    let theirs = product(&client, &admin, "globex").await;
    set(
        &client,
        &admin,
        "globex",
        &theirs,
        json!({ "care": "secret" }),
        200,
    )
    .await;

    // Their definition does not exist here, nor does their product.
    let ours = product(&client, &admin, "acme").await;
    set(&client, &admin, "acme", &ours, json!({ "care": "x" }), 422).await;
    client
        .get(&format!("/api/products/{theirs}/fields"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);
    client
        .delete("/api/fields/product/care")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);
    let defs: Value = client
        .get("/api/fields/product")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(defs, json!([]));
}
