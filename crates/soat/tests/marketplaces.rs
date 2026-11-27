//! I10: selling on marketplaces. Offers with a markup and a stock buffer,
//! orders brought in once, shipments and cancellations reported back.
#![cfg(feature = "marketplaces")]

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
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
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn address() -> Value {
    json!({ "first_name": "Erika", "last_name": "Muster", "line1": "Hauptstr. 1",
            "postal_code": "10115", "city": "Berlin", "country": "DE" })
}

#[tokio::test]
async fn marketplaces_offer_stock_and_bring_orders_in() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let loc = call(
        c,
        "acme",
        "POST",
        "/api/locations",
        a,
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    let product = |sku: &str, price: i64, status: &str| {
        json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR",
                "status": status })
    };
    let mug = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        product("MUG", 1200, "active"),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/inventory/{mug}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 5, "reason": "received" }),
        200,
    )
    .await;
    let cap = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        product("CAP", 800, "active"),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/inventory/{cap}"),
        a,
        json!({ "tracked": false }),
        200,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        product("SECRET", 100, "draft"),
        201,
    )
    .await;

    // Amazon at +15 %, one unit held back.
    call(
        c,
        "acme",
        "POST",
        "/api/marketplaces",
        a,
        json!({ "name": "X", "channel": "shopee" }),
        422,
    )
    .await;
    let m = call(
        c,
        "acme",
        "POST",
        "/api/marketplaces",
        a,
        json!({ "name": "Amazon DE", "channel": "amazon", "markup_bps": 1500,
                "stock_buffer": 1 }),
        201,
    )
    .await;
    assert_eq!(m["currency"], "EUR");
    let id = m["id"].as_str().unwrap().to_owned();
    let dup = call(
        c,
        "acme",
        "POST",
        "/api/marketplaces",
        a,
        json!({ "name": "Amazon DE", "channel": "amazon" }),
        409,
    )
    .await;
    assert_eq!(dup["code"], "name_taken");

    let o = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/offers"),
        a,
        Value::Null,
        200,
    )
    .await;
    let items = o["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{o}");
    let offer = |o: &Value, sku: &str| {
        o["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["sku"] == sku)
            .cloned()
    };
    let m_offer = offer(&o, "MUG").unwrap();
    assert_eq!(m_offer["price_minor"], 1380);
    assert_eq!(m_offer["quantity"], 4);
    assert_eq!(offer(&o, "CAP").unwrap()["quantity"], 100);

    // Its own SKU for the mug there; the cap left out.
    let l = call(
        c,
        "acme",
        "PUT",
        &format!("/api/marketplaces/{id}/listings"),
        a,
        json!({ "listings": [{ "sku": "MUG", "external_sku": "AMZ-MUG" },
                             { "item_id": cap, "listed": false }] }),
        200,
    )
    .await;
    assert_eq!(l["items"].as_array().unwrap().len(), 2);
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/marketplaces/{id}/listings"),
        a,
        json!({ "listings": [{ "sku": "SECRET" }] }),
        422,
    )
    .await;

    // The connector holds a key for marketplaces:sync only.
    let key = c
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "name": "amazon connector", "permissions": ["marketplaces:sync"] }))
        .send()
        .await
        .assert_status(201)
        .json::<Value>()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let k = Some(key.as_str());
    let o = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/offers"),
        k,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["items"].as_array().unwrap().len(), 1, "{o}");
    assert_eq!(o["items"][0]["external_sku"], "AMZ-MUG");
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/marketplaces/{id}"),
        k,
        json!({ "markup_bps": 0 }),
        403,
    )
    .await;

    // An order comes in, by the marketplace's SKU, paid; once only.
    let order = json!({ "external_id": "302-1234567", "email": "buyer@marketplace.example",
                        "shipping_address": address(),
                        "lines": [{ "sku": "AMZ-MUG", "quantity": 2, "unit_price_minor": 1380 }],
                        "shipping_price_minor": 490 });
    let first = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        order.clone(),
        201,
    )
    .await;
    assert_eq!(first["order"]["financial_status"], "paid", "{first}");
    assert_eq!(first["order"]["lines"][0]["unit_price_minor"], 1380);
    assert_eq!(first["order"]["shipping_minor"], 490);
    assert_eq!(first["state"], "open");
    assert_eq!(first["pending"], false);
    let order_id = first["order"]["id"].as_str().unwrap().to_owned();
    let again = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        order,
        200,
    )
    .await;
    assert_eq!(again["order"]["id"], order_id.as_str());
    // The stock is held: 5 - 2 - 1 held back.
    let o = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/offers"),
        k,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["items"][0]["quantity"], 2, "{o}");
    let bad = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        json!({ "external_id": "302-2", "shipping_address": address(),
                "lines": [{ "sku": "NOPE", "quantity": 1, "unit_price_minor": 100 }] }),
        422,
    )
    .await;
    assert!(bad.to_string().contains("NOPE"), "{bad}");
    call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        json!({ "external_id": "302-3", "shipping_address": address(),
                "lines": [{ "sku": "MUG", "quantity": 10, "unit_price_minor": 1380 }] }),
        409,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        json!({ "external_id": "302-4", "shipping_address": { "line1": "", "city": "" },
                "lines": [{ "sku": "MUG", "quantity": 1, "unit_price_minor": 1380 }] }),
        422,
    )
    .await;

    // Shipped in SOAT: the connector is told, with the tracking number.
    call(
        c,
        "acme",
        "POST",
        &format!("/api/orders/{order_id}/fulfillments"),
        a,
        json!({ "location_id": loc, "carrier": "DHL", "tracking_number": "00340434" }),
        201,
    )
    .await;
    let pending = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/orders?pending=true"),
        k,
        Value::Null,
        200,
    )
    .await;
    let items = pending["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{pending}");
    assert_eq!(items[0]["state"], "shipped");
    assert_eq!(items[0]["fulfillments"][0]["tracking_number"], "00340434");
    let m = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(m["orders"], 1);
    assert_eq!(m["pending"], 1);
    call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders/302-1234567/synced"),
        k,
        json!({ "state": "shipped" }),
        200,
    )
    .await;
    let pending = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/orders?pending=true"),
        k,
        Value::Null,
        200,
    )
    .await;
    assert!(pending["items"].as_array().unwrap().is_empty());
    call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders/302-1234567/cancel"),
        k,
        json!({}),
        409,
    )
    .await;

    // Canceled on the marketplace: the order is canceled, its stock freed.
    let second = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        json!({ "external_id": "302-5", "shipping_address": address(),
                "lines": [{ "item_id": mug, "quantity": 1, "unit_price_minor": 1380 }] }),
        201,
    )
    .await;
    let canceled = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders/302-5/cancel"),
        k,
        json!({ "reason": "buyer asked" }),
        200,
    )
    .await;
    assert_eq!(canceled["order"]["status"], "canceled");
    assert_eq!(canceled["pending"], false);
    assert_eq!(canceled["order"]["id"], second["order"]["id"]);
    let inv = call(
        c,
        "acme",
        "GET",
        &format!("/api/inventory/{mug}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(inv["on_hand"], 3, "{inv}");
    assert_eq!(inv["reserved"], 0, "{inv}");

    // Paused: nothing offered, no orders taken.
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/marketplaces/{id}"),
        a,
        json!({ "active": false }),
        200,
    )
    .await;
    let o = call(
        c,
        "acme",
        "GET",
        &format!("/api/marketplaces/{id}/offers"),
        k,
        Value::Null,
        200,
    )
    .await;
    assert!(o["items"].as_array().unwrap().is_empty());
    let paused = call(
        c,
        "acme",
        "POST",
        &format!("/api/marketplaces/{id}/orders"),
        k,
        json!({ "external_id": "302-6", "shipping_address": address(),
                "lines": [{ "sku": "MUG", "quantity": 1, "unit_price_minor": 1380 }] }),
        409,
    )
    .await;
    assert_eq!(paused["code"], "marketplace_paused");

    // The admin page lists it.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/marketplaces")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Amazon DE") && html.contains("paused"),
        "{html}"
    );
    let res = c
        .post("/admin/acme/marketplaces")
        .header("cookie", &cookie)
        .form(&json!({ "name": "Otto", "channel": "otto", "markup": "10", "buffer": "2" }))
        .send()
        .await;
    res.assert_status(303);
    let otto = call(c, "acme", "GET", "/api/marketplaces", a, Value::Null, 200).await;
    let otto = otto["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "Otto")
        .cloned()
        .unwrap();
    assert_eq!(otto["markup_bps"], 1000);
    assert_eq!(otto["stock_buffer"], 2);

    // Events, and another shop sees none of it.
    for (kind, n) in [
        ("marketplace.created", 2),
        ("marketplace_order.imported", 2),
        ("marketplace_order.synced", 1),
    ] {
        let e = call(
            c,
            "acme",
            "GET",
            &format!("/api/events?kind={kind}"),
            a,
            Value::Null,
            200,
        )
        .await;
        assert_eq!(e["items"].as_array().unwrap().len(), n, "{kind}");
    }
    let none = call(c, "other", "GET", "/api/marketplaces", a, Value::Null, 200).await;
    assert!(none["items"].as_array().unwrap().is_empty());
    for (method, path) in [
        ("GET", format!("/api/marketplaces/{id}")),
        ("GET", format!("/api/marketplaces/{id}/offers")),
        ("GET", format!("/api/marketplaces/{id}/orders")),
        ("PATCH", format!("/api/marketplaces/{id}")),
        ("DELETE", format!("/api/marketplaces/{id}")),
    ] {
        call(c, "other", method, &path, a, json!({}), 404).await;
    }
    call(
        c,
        "other",
        "POST",
        &format!("/api/marketplaces/{id}/orders/302-5/cancel"),
        a,
        json!({}),
        404,
    )
    .await;
    call(
        c,
        "acme",
        "DELETE",
        &format!("/api/marketplaces/{id}"),
        a,
        Value::Null,
        204,
    )
    .await;
    // Its orders stay.
    call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{order_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
}
