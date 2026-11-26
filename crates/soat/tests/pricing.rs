//! B10: price lists and currencies.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    token: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 || status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

async fn product(client: &TestClient, token: &str, shop: &str, price: i64) -> String {
    let p = send(
        client,
        token,
        shop,
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": price, "currency": "EUR" }),
        201,
    )
    .await;
    p["id"].as_str().unwrap().to_owned()
}

async fn list(
    client: &TestClient,
    token: &str,
    name: &str,
    currency: &str,
    priority: i32,
) -> String {
    let l = send(
        client,
        token,
        "acme",
        "POST",
        "/api/price-lists",
        json!({ "name": name, "currency": currency, "priority": priority }),
        201,
    )
    .await;
    l["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_best_active_list_sets_the_price() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let item = product(&client, &token, "acme", 2000).await;
    let price = |currency: &str, status: u16| {
        let (client, token, item) = (&client, &token, &item);
        let path = format!("/api/prices/{item}?currency={currency}");
        async move { send(client, token, "acme", "GET", &path, Value::Null, status).await }
    };

    // Its own price in its own currency; nothing in CHF yet.
    let p = price("EUR", 200).await;
    assert_eq!(
        (p["price_minor"].as_i64(), p["price_list_id"].as_str()),
        (Some(2000), None)
    );
    price("CHF", 404).await;
    price("eur", 422).await;

    let swiss = list(&client, &token, "Switzerland", "CHF", 0).await;
    let sale = list(&client, &token, "Sale", "EUR", 10).await;
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{swiss}/prices"),
        json!({ "prices": [{ "item_id": item, "price_minor": 2200 }] }),
        200,
    )
    .await;
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{sale}/prices"),
        json!({ "prices": [{ "item_id": item, "price_minor": 1500, "compare_at_minor": 2000 }] }),
        200,
    )
    .await;

    assert_eq!(price("CHF", 200).await["price_minor"], 2200);
    let p = price("EUR", 200).await;
    assert_eq!(
        (p["price_minor"].as_i64(), p["compare_at_minor"].as_i64()),
        (Some(1500), Some(2000))
    );
    assert_eq!(p["price_list_id"], sale.as_str());

    // A higher-priority list wins; switching it off falls back.
    let vip = list(&client, &token, "VIP", "EUR", 20).await;
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{vip}/prices"),
        json!({ "prices": [{ "item_id": item, "price_minor": 1000 }] }),
        200,
    )
    .await;
    assert_eq!(price("EUR", 200).await["price_minor"], 1000);
    send(
        &client,
        &token,
        "acme",
        "PATCH",
        &format!("/api/price-lists/{vip}"),
        json!({ "active": false }),
        200,
    )
    .await;
    assert_eq!(price("EUR", 200).await["price_minor"], 1500);
    send(
        &client,
        &token,
        "acme",
        "DELETE",
        &format!("/api/price-lists/{sale}/prices/{item}"),
        Value::Null,
        204,
    )
    .await;
    assert_eq!(price("EUR", 200).await["price_minor"], 2000);

    // Bad prices change nothing.
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{swiss}/prices"),
        json!({ "prices": [{ "item_id": item, "price_minor": 100, "compare_at_minor": 100 }] }),
        422,
    )
    .await;
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{swiss}/prices"),
        json!({ "prices": [{ "item_id": "prod_nope", "price_minor": 1 }] }),
        422,
    )
    .await;
    let prices = send(
        &client,
        &token,
        "acme",
        "GET",
        &format!("/api/price-lists/{swiss}/prices"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(prices["items"][0]["price_minor"], 2200);

    // Deleting the product drops its list prices once events run.
    send(
        &client,
        &token,
        "acme",
        "DELETE",
        &format!("/api/products/{item}"),
        Value::Null,
        204,
    )
    .await;
    soat_events::dispatch_resources(&client.resources().await)
        .await
        .unwrap();
    let prices = send(
        &client,
        &token,
        "acme",
        "GET",
        &format!("/api/price-lists/{swiss}/prices"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(prices["total"], 0);
}

#[tokio::test]
async fn price_lists_stay_in_their_shop() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = send(
        &client,
        &token,
        "globex",
        "POST",
        "/api/price-lists",
        json!({ "name": "G", "currency": "EUR" }),
        201,
    )
    .await;
    let theirs = theirs["id"].as_str().unwrap();
    let their_item = product(&client, &token, "globex", 100).await;
    let ours = list(&client, &token, "Ours", "EUR", 0).await;

    send(
        &client,
        &token,
        "acme",
        "GET",
        &format!("/api/price-lists/{theirs}"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{theirs}/prices"),
        json!({ "prices": [] }),
        404,
    )
    .await;
    // Their item cannot be priced in our list, nor priced from here.
    send(
        &client,
        &token,
        "acme",
        "PUT",
        &format!("/api/price-lists/{ours}/prices"),
        json!({ "prices": [{ "item_id": their_item, "price_minor": 1 }] }),
        422,
    )
    .await;
    send(
        &client,
        &token,
        "acme",
        "GET",
        &format!("/api/prices/{their_item}?currency=EUR"),
        Value::Null,
        404,
    )
    .await;
}
