//! D3 (part): the catalog as buyers see it.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn admin(
    client: &TestClient,
    t: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    if status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn shop(client: &TestClient, tenant: &str, path: &str, status: u16) -> Value {
    let res = client
        .get(path)
        .header("x-soat-tenant", tenant)
        .send()
        .await;
    res.assert_status(status);
    if status == 200 {
        res.json()
    } else {
        Value::Null
    }
}

fn product(sku: &str, title: &str, price: i64) -> Value {
    json!({ "sku": sku, "title": title, "price_minor": price, "currency": "EUR" })
}

#[tokio::test]
async fn buyers_see_active_products_by_handle() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let mut shoe = product("SHOE", "Trail Shoe", 12999);
    shoe["description"] = json!("Light and *grippy*.");
    let shoe = admin(&client, &t, "POST", "/api/products", shoe, 201).await;
    assert_eq!(shoe["handle"], "trail-shoe");
    assert_eq!(shoe["status"], "active");
    // The same title gets a handle of its own.
    let twin = admin(
        &client,
        &t,
        "POST",
        "/api/products",
        product("SHOE2", "Trail Shoe", 1),
        201,
    )
    .await;
    assert!(
        twin["handle"].as_str().unwrap().starts_with("trail-shoe-"),
        "{twin}"
    );
    // A handle someone has is a 409; a bad one a 422.
    let mut dup = product("SHOE3", "X", 1);
    dup["handle"] = json!("trail-shoe");
    admin(&client, &t, "POST", "/api/products", dup, 409).await;
    let mut bad = product("SHOE4", "X", 1);
    bad["handle"] = json!("Trail Shoe");
    admin(&client, &t, "POST", "/api/products", bad, 422).await;
    let mut bad = product("SHOE5", "X", 1);
    bad["status"] = json!("hidden");
    admin(&client, &t, "POST", "/api/products", bad, 422).await;

    let mut hat = product("HAT", "Wool Hat", 2500);
    hat["status"] = json!("draft");
    let hat = admin(&client, &t, "POST", "/api/products", hat, 201).await;
    let drafts = admin(
        &client,
        &t,
        "GET",
        "/api/products?status=draft",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(drafts["total"], 1);

    // Options and variants come along.
    let id = shoe["id"].as_str().unwrap();
    admin(
        &client,
        &t,
        "PUT",
        &format!("/api/products/{id}/options"),
        json!({ "options": [{ "name": "Size", "values": ["42", "43"] }] }),
        200,
    )
    .await;
    admin(
        &client,
        &t,
        "POST",
        &format!("/api/products/{id}/variants"),
        json!({ "sku": "SHOE-42", "options": ["42"] }),
        201,
    )
    .await;

    let list = shop(&client, "acme", "/api/storefront/products", 200).await;
    assert_eq!(list["total"], 2, "drafts stay hidden: {list}");
    let page = shop(&client, "acme", "/api/storefront/products/trail-shoe", 200).await;
    assert_eq!(page["description"], "Light and *grippy*.");
    assert_eq!(page["options"][0]["values"], json!(["42", "43"]));
    assert_eq!(page["variants"][0]["title"], "42");
    assert_eq!(page["images"], json!([]));
    shop(&client, "acme", "/api/storefront/products/wool-hat", 404).await;
    shop(
        &client,
        "globex",
        "/api/storefront/products/trail-shoe",
        404,
    )
    .await;
    assert_eq!(
        shop(&client, "globex", "/api/storefront/products", 200).await["total"],
        0
    );

    // Publishing the draft shows it; archiving the shoe hides it.
    let hat_id = hat["id"].as_str().unwrap();
    admin(
        &client,
        &t,
        "PATCH",
        &format!("/api/products/{hat_id}"),
        json!({ "status": "active" }),
        200,
    )
    .await;
    admin(
        &client,
        &t,
        "PATCH",
        &format!("/api/products/{id}"),
        json!({ "status": "archived" }),
        200,
    )
    .await;
    shop(&client, "acme", "/api/storefront/products/wool-hat", 200).await;
    shop(&client, "acme", "/api/storefront/products/trail-shoe", 404).await;

    // Collections by handle, and their products.
    let col = admin(
        &client,
        &t,
        "POST",
        "/api/collections",
        json!({ "title": "Winter" }),
        201,
    )
    .await;
    let col_id = col["id"].as_str().unwrap();
    admin(
        &client,
        &t,
        "PUT",
        &format!("/api/collections/{col_id}/products/{hat_id}"),
        Value::Null,
        204,
    )
    .await;
    let c = shop(&client, "acme", "/api/storefront/collections/winter", 200).await;
    assert_eq!(c["title"], "Winter");
    let in_col = shop(
        &client,
        "acme",
        "/api/storefront/products?collection=winter",
        200,
    )
    .await;
    assert_eq!(in_col["total"], 1);
    assert_eq!(in_col["items"][0]["handle"], "wool-hat");
    assert_eq!(in_col["items"][0]["image"], Value::Null);
    shop(&client, "globex", "/api/storefront/collections/winter", 404).await;
    let found = shop(
        &client,
        "acme",
        "/api/storefront/products?q=wool&sort=price_minor",
        200,
    )
    .await;
    assert_eq!(found["total"], 1);
}
