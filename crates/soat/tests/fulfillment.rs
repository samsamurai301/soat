//! C11: shipping orders.

mod common;

use serde_json::{json, Value};
use soat_core::Currency;
use soat_orders::{place, NewOrder, NewOrderLine};
use soat_tenancy::Scope;
use zapline::db::sqlx::{self, PgPool};
use zapline::test::TestClient;

struct In(&'static str);

impl Scope for In {
    fn tenant_id(&self) -> &str {
        self.0
    }
    fn actor_id(&self) -> Option<&str> {
        None
    }
}

fn line(item: &str, quantity: i64) -> NewOrderLine {
    NewOrderLine {
        item_id: item.into(),
        product_id: Some(item.into()),
        sku: item.into(),
        title: item.into(),
        quantity,
        unit_price_minor: 1000,
        tax_class: "standard".into(),
        tax_rate_bps: 0,
        tax_minor: 0,
        discount_minor: 0,
    }
}

async fn placed(url: &str, lines: Vec<NewOrderLine>) -> String {
    let pool = PgPool::connect(url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let order = NewOrder {
        cart_id: None,
        customer_id: None,
        email: "a@example.com".into(),
        currency: Currency::parse("EUR").unwrap(),
        lines,
        shipping: None,
        shipping_address: None,
        billing_address: None,
        prices_include_tax: true,
        tax_country: None,
        reverse_charge: false,
        vat_number: None,
        note: String::new(),
        discount_code: None,
    };
    let (o, _) = place(&mut tx, &In("acme"), &order).await.unwrap();
    tx.commit().await.unwrap();
    o.id
}

async fn send(
    client: &TestClient,
    shop: &str,
    t: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

async fn on_hand(client: &TestClient, t: &str, item: &str) -> Vec<i64> {
    let v = send(
        client,
        "acme",
        t,
        "GET",
        &format!("/api/inventory/{item}"),
        Value::Null,
        200,
    )
    .await;
    let mut levels: Vec<(String, i64)> = v["levels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["location_id"].as_str().unwrap().to_owned(),
                l["on_hand"].as_i64().unwrap(),
            )
        })
        .collect();
    levels.sort();
    levels.into_iter().map(|(_, n)| n).collect()
}

#[tokio::test]
async fn orders_ship_in_parts_and_take_stock() {
    let (client, url) = common::client_and_url().await;
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "globex"]).await;
    let t = t.as_str();
    let mut locs = Vec::new();
    for name in ["A", "B"] {
        let l = send(
            &client,
            "acme",
            t,
            "POST",
            "/api/locations",
            json!({ "name": name, "country": "DE" }),
            201,
        )
        .await;
        locs.push(l["id"].as_str().unwrap().to_owned());
    }
    let product =
        |sku: &str| json!({ "sku": sku, "title": sku, "price_minor": 1000, "currency": "EUR" });
    let tee = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/products",
        product("TEE"),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let book = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/products",
        product("BOOK"),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (loc, units) in [(&locs[0], 5), (&locs[1], 1)] {
        send(
            &client,
            "acme",
            t,
            "POST",
            &format!("/api/inventory/{tee}/adjustments"),
            json!({ "location_id": loc, "delta": units, "reason": "received" }),
            200,
        )
        .await;
    }
    send(
        &client,
        "acme",
        t,
        "PATCH",
        &format!("/api/inventory/{book}"),
        json!({ "tracked": false }),
        200,
    )
    .await;
    let order = placed(&url, vec![line(&tee, 3), line(&book, 1)]).await;
    send(
        &client,
        "acme",
        t,
        "POST",
        "/api/inventory/reservations",
        json!({ "item_id": tee, "quantity": 3, "reference": order }),
        201,
    )
    .await;
    let o = send(
        &client,
        "acme",
        t,
        "GET",
        &format!("/api/orders/{order}"),
        Value::Null,
        200,
    )
    .await;
    let tee_line = o["lines"][0]["id"].as_str().unwrap().to_owned();
    let ship = format!("/api/orders/{order}/fulfillments");

    let o = send(
        &client,
        "acme",
        t,
        "POST",
        &ship,
        json!({ "lines": [{ "line_id": tee_line, "quantity": 2 }], "carrier": "dhl" }),
        201,
    )
    .await;
    assert_eq!(o["fulfillment_status"], "partially_fulfilled");
    assert_eq!(o["lines"][0]["fulfilled_quantity"], 2);
    assert_eq!(
        o["fulfillments"][0]["lines"][0]["location_id"],
        locs[0].as_str()
    );
    assert_eq!(on_hand(&client, t, &tee).await, [3, 1]);
    let held = send(
        &client,
        "acme",
        t,
        "GET",
        &format!("/api/inventory/reservations?reference={order}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(held["items"][0]["quantity"], 1);

    send(
        &client,
        "acme",
        t,
        "POST",
        &ship,
        json!({ "lines": [{ "line_id": tee_line, "quantity": 2 }] }),
        422,
    )
    .await;
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/cancel"),
        json!({}),
        409,
    )
    .await;

    let o = send(
        &client,
        "acme",
        t,
        "POST",
        &ship,
        json!({ "location_id": locs[1] }),
        201,
    )
    .await;
    assert_eq!(o["fulfillment_status"], "fulfilled");
    assert!(o["fulfillments"][1]["lines"][1]["location_id"].is_null());
    assert_eq!(on_hand(&client, t, &tee).await, [3, 0]);
    let held = send(
        &client,
        "acme",
        t,
        "GET",
        &format!("/api/inventory/reservations?reference={order}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(held["total"], 0);
    send(&client, "acme", t, "POST", &ship, json!({}), 422).await;
    let events = send(
        &client,
        "acme",
        t,
        "GET",
        "/api/events?kind=order.fulfilled",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 1);

    let first = o["fulfillments"][0]["id"].as_str().unwrap().to_owned();
    let second = o["fulfillments"][1]["id"].as_str().unwrap().to_owned();
    let o = send(
        &client,
        "acme",
        t,
        "PATCH",
        &format!("/api/fulfillments/{first}"),
        json!({ "tracking_number": "00340434", "tracking_url": "https://track.example/00340434" }),
        200,
    )
    .await;
    assert_eq!(o["fulfillments"][0]["tracking_number"], "00340434");
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/fulfillments/{first}/delivered"),
        Value::Null,
        200,
    )
    .await;
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/fulfillments/{first}/cancel"),
        Value::Null,
        409,
    )
    .await;

    // Canceling a shipment puts its units back where they came from.
    let o = send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/fulfillments/{second}/cancel"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["fulfillment_status"], "partially_fulfilled");
    assert_eq!(o["fulfillments"][1]["status"], "canceled");
    assert_eq!(on_hand(&client, t, &tee).await, [3, 1]);

    // Another shop reaches none of it.
    send(&client, "globex", t, "POST", &ship, json!({}), 404).await;
    send(
        &client,
        "globex",
        t,
        "PATCH",
        &format!("/api/fulfillments/{first}"),
        json!({ "carrier": "x" }),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        t,
        "POST",
        &format!("/api/fulfillments/{second}/cancel"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "globex",
        t,
        "POST",
        &format!("/api/fulfillments/{first}/delivered"),
        Value::Null,
        404,
    )
    .await;
}
