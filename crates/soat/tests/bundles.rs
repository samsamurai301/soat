//! B12: bundles sell a set of items from their stock.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 300 {
        Value::Null
    } else {
        res.json()
    }
}

fn berlin() -> Value {
    json!({ "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" })
}

#[tokio::test]
async fn bundles_sell_from_their_items_stock() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let staff = |method: &'static str, path: String, body: Value, status: u16| {
        let (client, admin) = (&client, &admin);
        async move { call(client, "acme", method, &path, admin, body, status).await }
    };
    let loc = staff(
        "POST",
        "/api/locations".into(),
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    let product = |sku: &'static str, price: i64, units: i64| {
        let staff = &staff;
        let loc = loc.clone();
        async move {
            let id = staff(
                "POST",
                "/api/products".into(),
                json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR" }),
                201,
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned();
            if units > 0 {
                staff(
                    "POST",
                    format!("/api/inventory/{id}/adjustments"),
                    json!({ "location_id": loc, "delta": units, "reason": "received" }),
                    200,
                )
                .await;
            }
            id
        }
    };
    let tee = product("TEE", 1500, 10).await;
    let cap = product("CAP", 1000, 3).await;
    let kit = product("KIT", 3500, 0).await;

    let path = format!("/api/products/{kit}/bundle");
    for bad in [
        json!({ "items": [{ "item_id": kit, "quantity": 1 }] }),
        json!({ "items": [{ "item_id": tee, "quantity": 0 }] }),
        json!({ "items": [{ "item_id": tee, "quantity": 1 }, { "item_id": tee, "quantity": 2 }] }),
        json!({ "items": [{ "item_id": "prod_nope", "quantity": 1 }] }),
    ] {
        staff("PUT", path.clone(), bad, 422).await;
    }
    let set = staff(
        "PUT",
        path.clone(),
        json!({ "items": [{ "item_id": tee, "quantity": 2 }, { "item_id": cap, "quantity": 1 }] }),
        200,
    )
    .await;
    assert_eq!(set["items"].as_array().unwrap().len(), 2);
    assert_eq!(staff("GET", path.clone(), Value::Null, 200).await, set);
    // No bundles in bundles, either way round.
    let box_ = product("BOX", 9000, 0).await;
    staff(
        "PUT",
        format!("/api/products/{box_}/bundle"),
        json!({ "items": [{ "item_id": kit, "quantity": 1 }] }),
        422,
    )
    .await;
    staff(
        "PUT",
        format!("/api/products/{tee}/bundle"),
        json!({ "items": [{ "item_id": box_, "quantity": 1 }] }),
        422,
    )
    .await;

    // 10 tees make 5 kits, but 3 caps make 3.
    let stock = staff("GET", format!("/api/inventory/{kit}"), Value::Null, 200).await;
    assert_eq!(stock["available"], 3);

    let d = staff(
        "POST",
        "/api/draft-orders".into(),
        json!({ "currency": "EUR", "email": "ada@example.com", "shipping_address": berlin(),
                "lines": [{ "item_id": kit, "quantity": 4 }] }),
        201,
    )
    .await;
    let draft = d["id"].as_str().unwrap().to_owned();
    staff(
        "POST",
        format!("/api/draft-orders/{draft}/complete"),
        json!({ "paid": true }),
        409,
    )
    .await;
    staff(
        "PUT",
        format!("/api/draft-orders/{draft}"),
        json!({ "currency": "EUR", "email": "ada@example.com", "shipping_address": berlin(),
                "lines": [{ "item_id": kit, "quantity": 2 }] }),
        200,
    )
    .await;
    let done = staff(
        "POST",
        format!("/api/draft-orders/{draft}/complete"),
        json!({ "paid": true }),
        201,
    )
    .await;
    let order = done["order"]["id"].as_str().unwrap().to_owned();
    assert_eq!(done["order"]["total_minor"], 7000);
    let held = staff(
        "GET",
        format!("/api/inventory/reservations?reference={order}"),
        Value::Null,
        200,
    )
    .await;
    let mut held: Vec<(String, i64)> = held["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["item_id"].as_str().unwrap().to_owned(),
                r["quantity"].as_i64().unwrap(),
            )
        })
        .collect();
    held.sort();
    let mut want = vec![(tee.clone(), 4), (cap.clone(), 2)];
    want.sort();
    assert_eq!(held, want);
    let stock = staff("GET", format!("/api/inventory/{kit}"), Value::Null, 200).await;
    assert_eq!(stock["available"], 1);

    // Shipping the kits takes their items out of stock.
    let shipped = staff(
        "POST",
        format!("/api/orders/{order}/fulfillments"),
        json!({}),
        201,
    )
    .await;
    assert_eq!(shipped["fulfillment_status"], "fulfilled");
    let t = staff("GET", format!("/api/inventory/{tee}"), Value::Null, 200).await;
    let c = staff("GET", format!("/api/inventory/{cap}"), Value::Null, 200).await;
    assert_eq!(
        (t["on_hand"].as_i64(), t["reserved"].as_i64()),
        (Some(6), Some(0))
    );
    assert_eq!(
        (c["on_hand"].as_i64(), c["reserved"].as_i64()),
        (Some(1), Some(0))
    );

    // Canceling the shipment puts the items back.
    let f = shipped["fulfillments"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    staff(
        "POST",
        format!("/api/fulfillments/{f}/cancel"),
        Value::Null,
        200,
    )
    .await;
    let c = staff("GET", format!("/api/inventory/{cap}"), Value::Null, 200).await;
    assert_eq!(c["on_hand"], 3);

    // An empty list makes the kit an ordinary product.
    staff("PUT", path.clone(), json!({ "items": [] }), 200).await;
    assert_eq!(
        staff("GET", path, Value::Null, 200).await["items"],
        json!([])
    );
}

#[tokio::test]
async fn bundles_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = call(
        &client,
        "globex",
        "POST",
        "/api/products",
        &admin,
        json!({ "sku": "KIT", "title": "Kit", "price_minor": 1, "currency": "EUR" }),
        201,
    )
    .await;
    let theirs = theirs["id"].as_str().unwrap();
    let ours = call(
        &client,
        "acme",
        "POST",
        "/api/products",
        &admin,
        json!({ "sku": "KIT", "title": "Kit", "price_minor": 1, "currency": "EUR" }),
        201,
    )
    .await;
    let ours = ours["id"].as_str().unwrap();
    let path = format!("/api/products/{theirs}/bundle");
    call(&client, "acme", "GET", &path, &admin, Value::Null, 404).await;
    // Their product is no item of ours.
    call(
        &client,
        "acme",
        "PUT",
        &format!("/api/products/{ours}/bundle"),
        &admin,
        json!({ "items": [{ "item_id": theirs, "quantity": 1 }] }),
        422,
    )
    .await;
}
