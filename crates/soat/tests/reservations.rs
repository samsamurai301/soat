//! B9: stock reservations.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
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

/// A product with `units` on hand, returning its id.
async fn stocked(client: &TestClient, token: &str, units: i64) -> String {
    let loc = send(
        client,
        token,
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await;
    let p = send(
        client,
        token,
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 1, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap().to_owned();
    send(
        client,
        token,
        "POST",
        &format!("/api/inventory/{id}/adjustments"),
        json!({ "location_id": loc["id"], "delta": units, "reason": "received" }),
        200,
    )
    .await;
    id
}

fn hold(item: &str, quantity: i64, reference: &str) -> Value {
    json!({ "item_id": item, "quantity": quantity, "reference": reference })
}

#[tokio::test]
async fn reservations_hold_available_stock() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let item = stocked(&client, &token, 5).await;
    let r = send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 3, "cart_a"),
        201,
    )
    .await;
    assert!(r["id"].as_str().unwrap().starts_with("rsv_"));
    assert!(r["expires_at"].is_null());
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 3, "cart_b"),
        409,
    )
    .await;
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 2, "cart_b"),
        201,
    )
    .await;

    let v = send(
        &client,
        &token,
        "GET",
        &format!("/api/inventory/{item}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        (
            v["on_hand"].as_i64(),
            v["reserved"].as_i64(),
            v["available"].as_i64()
        ),
        (Some(5), Some(5), Some(0))
    );

    // Releasing a cart frees its units; releasing again is harmless.
    for _ in 0..2 {
        send(
            &client,
            &token,
            "POST",
            "/api/inventory/reservations/release",
            json!({ "reference": "cart_a" }),
            204,
        )
        .await;
    }
    let v = send(
        &client,
        &token,
        "GET",
        &format!("/api/inventory/{item}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(v["available"], 3);

    let list = send(
        &client,
        &token,
        "GET",
        "/api/inventory/reservations?reference=cart_b",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 1);
    let id = list["items"][0]["id"].as_str().unwrap();
    send(
        &client,
        &token,
        "DELETE",
        &format!("/api/inventory/reservations/{id}"),
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        &token,
        "DELETE",
        &format!("/api/inventory/reservations/{id}"),
        Value::Null,
        404,
    )
    .await;

    // Backorders and untracked items can always be reserved.
    send(
        &client,
        &token,
        "PATCH",
        &format!("/api/inventory/{item}"),
        json!({ "allow_backorder": true }),
        200,
    )
    .await;
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 50, "cart_c"),
        201,
    )
    .await;
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold("prod_nope", 1, "cart_c"),
        422,
    )
    .await;
}

#[tokio::test]
async fn expired_reservations_stop_counting() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let item = stocked(&client, &token, 1).await;
    let mut body = hold(&item, 1, "cart_a");
    body["ttl_seconds"] = json!(1);
    let r = send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        body,
        201,
    )
    .await;
    assert!(r["expires_at"].as_str().unwrap().ends_with('Z'));
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 1, "cart_b"),
        409,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    send(
        &client,
        &token,
        "POST",
        "/api/inventory/reservations",
        hold(&item, 1, "cart_b"),
        201,
    )
    .await;
}

#[tokio::test]
async fn racing_carts_never_oversell() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let item = stocked(&client, &token, 3).await;
    let tries = (0..8).map(|i| {
        let client = &client;
        let token = &token;
        let item = &item;
        async move {
            client
                .post("/api/inventory/reservations")
                .header("x-soat-tenant", "acme")
                .bearer(token)
                .json(&hold(item, 1, &format!("cart_{i}")))
                .send()
                .await
                .status()
        }
    });
    let statuses = futures_util::future::join_all(tries).await;
    let won = statuses.iter().filter(|s| s.as_u16() == 201).count();
    assert_eq!(won, 3, "{statuses:?}");
}
