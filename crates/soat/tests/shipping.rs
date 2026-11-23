//! C6: shipping zones and rates.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    shop: &str,
    token: &str,
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

async fn zone(client: &TestClient, t: &str, name: &str, countries: Value) -> String {
    let z = send(
        client,
        "acme",
        t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": name, "countries": countries }),
        201,
    )
    .await;
    z["id"].as_str().unwrap().to_owned()
}

async fn rate(client: &TestClient, t: &str, zone: &str, body: Value) -> String {
    let r = send(
        client,
        "acme",
        t,
        "POST",
        &format!("/api/shipping/zones/{zone}/rates"),
        body,
        201,
    )
    .await;
    r["id"].as_str().unwrap().to_owned()
}

async fn names(client: &TestClient, t: &str, q: Value) -> Vec<String> {
    let o = send(client, "acme", t, "POST", "/api/shipping/options", q, 200).await;
    o["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
}

fn order(country: &str, subtotal: i64, grams: i64) -> Value {
    json!({ "country": country, "currency": "EUR", "subtotal_minor": subtotal, "weight_grams": grams })
}

#[tokio::test]
async fn rates_apply_by_country_value_and_weight() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let de = zone(&client, &t, "Domestic", json!(["de"])).await;
    let eu = zone(&client, &t, "EU", json!(["FR", "AT"])).await;
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": "Clash", "countries": ["AT"] }),
        409,
    )
    .await;

    rate(
        &client,
        &t,
        &de,
        json!({ "name": "Standard", "currency": "EUR", "price_minor": 495,
                "max_subtotal_minor": 4999, "max_weight_grams": 5000, "min_days": 1, "max_days": 3 }),
    )
    .await;
    rate(
        &client,
        &t,
        &de,
        json!({ "name": "Free", "currency": "EUR", "price_minor": 0, "min_subtotal_minor": 5000 }),
    )
    .await;
    let express = rate(
        &client,
        &t,
        &de,
        json!({ "name": "Express", "carrier": "dhl", "currency": "EUR", "price_minor": 1290 }),
    )
    .await;
    rate(
        &client,
        &t,
        &eu,
        json!({ "name": "EU Standard", "currency": "EUR", "price_minor": 990 }),
    )
    .await;
    rate(
        &client,
        &t,
        &eu,
        json!({ "name": "EU CHF", "currency": "CHF", "price_minor": 990 }),
    )
    .await;

    assert_eq!(
        names(&client, &t, order("DE", 2000, 1000)).await,
        ["Standard", "Express"]
    );
    assert_eq!(
        names(&client, &t, order("de", 5000, 1000)).await,
        ["Free", "Express"]
    );
    assert_eq!(
        names(&client, &t, order("DE", 2000, 9000)).await,
        ["Express"]
    );
    assert_eq!(
        names(&client, &t, order("AT", 2000, 0)).await,
        ["EU Standard"]
    );
    assert!(names(&client, &t, order("US", 2000, 0)).await.is_empty());

    // Replacing a rate; inactive rates are not offered.
    send(
        &client,
        "acme",
        &t,
        "PUT",
        &format!("/api/shipping/rates/{express}"),
        json!({ "name": "Express", "currency": "EUR", "price_minor": 1290, "active": false }),
        200,
    )
    .await;
    assert_eq!(
        names(&client, &t, order("DE", 2000, 1000)).await,
        ["Standard"]
    );

    let z = send(
        &client,
        "acme",
        &t,
        "GET",
        &format!("/api/shipping/zones/{de}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(z["rates"].as_array().unwrap().len(), 3);
    assert_eq!(z["rates"][0]["name"], "Free");

    for bad in [
        json!({ "name": "X", "currency": "EUR", "price_minor": 1, "min_subtotal_minor": 10, "max_subtotal_minor": 5 }),
        json!({ "name": "X", "currency": "euro", "price_minor": 1 }),
        json!({ "name": "X", "currency": "EUR", "price_minor": -1 }),
        json!({ "name": "X", "currency": "EUR", "price_minor": 1, "tax_class": "Bad" }),
    ] {
        send(
            &client,
            "acme",
            &t,
            "POST",
            &format!("/api/shipping/zones/{de}/rates"),
            bad,
            422,
        )
        .await;
    }

    send(
        &client,
        "acme",
        &t,
        "DELETE",
        &format!("/api/shipping/rates/{express}"),
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        "acme",
        &t,
        "DELETE",
        &format!("/api/shipping/rates/{express}"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "acme",
        &t,
        "PATCH",
        &format!("/api/shipping/zones/{eu}"),
        json!({ "countries": ["FR"] }),
        200,
    )
    .await;
    assert!(names(&client, &t, order("AT", 2000, 0)).await.is_empty());
    send(
        &client,
        "acme",
        &t,
        "DELETE",
        &format!("/api/shipping/zones/{de}"),
        Value::Null,
        204,
    )
    .await;
    assert!(names(&client, &t, order("DE", 2000, 0)).await.is_empty());
}

#[tokio::test]
async fn products_carry_a_weight() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let p = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "S", "title": "S", "price_minor": 1, "currency": "EUR", "weight_grams": 350 }),
        201,
    )
    .await;
    assert_eq!(p["weight_grams"], 350);
    let p = send(
        &client,
        "acme",
        &t,
        "PATCH",
        &format!("/api/products/{}", p["id"].as_str().unwrap()),
        json!({ "weight_grams": 400 }),
        200,
    )
    .await;
    assert_eq!(p["weight_grams"], 400);
}

#[tokio::test]
async fn another_shops_zones_and_rates_are_not_found() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let z = zone(&client, &t, "DE", json!(["DE"])).await;
    let r = rate(
        &client,
        &t,
        &z,
        json!({ "name": "S", "currency": "EUR", "price_minor": 1 }),
    )
    .await;
    let zp = format!("/api/shipping/zones/{z}");
    send(&client, "globex", &t, "GET", &zp, Value::Null, 404).await;
    send(
        &client,
        "globex",
        &t,
        "PATCH",
        &zp,
        json!({ "name": "Mine" }),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        &t,
        "POST",
        &format!("{zp}/rates"),
        json!({ "name": "S", "currency": "EUR", "price_minor": 1 }),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        &t,
        "PUT",
        &format!("/api/shipping/rates/{r}"),
        json!({ "name": "S", "currency": "EUR", "price_minor": 0 }),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        &t,
        "DELETE",
        &format!("/api/shipping/rates/{r}"),
        Value::Null,
        404,
    )
    .await;
    send(&client, "globex", &t, "DELETE", &zp, Value::Null, 404).await;
    let o = send(
        &client,
        "globex",
        &t,
        "POST",
        "/api/shipping/options",
        order("DE", 1, 0),
        200,
    )
    .await;
    assert!(o["items"].as_array().unwrap().is_empty());
}
