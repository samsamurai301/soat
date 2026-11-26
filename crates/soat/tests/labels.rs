//! C19: shipping labels through a carrier account (Sendcloud).
#![cfg(feature = "sendcloud")]

mod common;

use serde_json::{json, Value};
use soat_testing::{fake_api, Log, Received};
use zapline::db::Database;
use zapline::test::TestClient;

fn answer(r: &Received) -> (u16, Value) {
    let path = r.path.split('?').next().unwrap_or_default();
    match (r.method.as_str(), path) {
        ("POST", "/parcels") if r.json()["parcel"]["postal_code"] == "00000" => (
            400,
            json!({ "error": { "code": 400, "message": "Postal code is invalid" } }),
        ),
        ("POST", "/parcels") => (
            200,
            json!({ "parcel": { "id": 4711, "tracking_number": "JVGL0123",
                    "tracking_url": "https://tracking.example/JVGL0123",
                    "carrier": { "code": "dhl" } } }),
        ),
        ("GET", "/labels/label_printer/4711") => (
            200,
            json!({ "raw": "%PDF-1.4 label", "content_type": "application/pdf" }),
        ),
        ("POST", "/parcels/4711/cancel") => (200, json!({ "status": "deleted" })),
        _ => (404, json!({ "error": { "message": "not found" } })),
    }
}

async fn setup() -> (TestClient, String, Log) {
    let (base, log) = fake_api(answer).await;
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.sendcloud = Some(soat_sendcloud::Sendcloud::new(&base));
    let client = soat::app(Database::postgres(url), settings).test();
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, &["acme", "globex"]).await;
    (client, token, log)
}

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

/// A shipped order of two 500 g tees to Berlin; returns the order and
/// shipment ids.
async fn shipment(client: &TestClient, t: &str, shop: &str, postal_code: &str) -> (String, String) {
    let tee = send(
        client,
        t,
        shop,
        "POST",
        "/api/products",
        json!({ "sku": format!("TEE-{postal_code}"), "title": "Tee", "price_minor": 1500, "currency": "EUR",
                "weight_grams": 500 }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        client,
        t,
        shop,
        "PATCH",
        &format!("/api/inventory/{tee}"),
        json!({ "tracked": false }),
        200,
    )
    .await;
    let d = send(
        client,
        t,
        shop,
        "POST",
        "/api/draft-orders",
        json!({ "currency": "EUR", "email": "ada@example.com",
                "shipping_address": { "first_name": "Ada", "last_name": "L", "line1": "Hauptstr. 1",
                                      "postal_code": postal_code, "city": "Berlin", "country": "DE" },
                "lines": [{ "item_id": tee, "quantity": 2 }] }),
        201,
    )
    .await;
    let done = send(
        client,
        t,
        shop,
        "POST",
        &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
        json!({ "paid": true }),
        201,
    )
    .await;
    let order = done["order"]["id"].as_str().unwrap();
    let o = send(
        client,
        t,
        shop,
        "POST",
        &format!("/api/orders/{order}/fulfillments"),
        json!({}),
        201,
    )
    .await;
    (
        order.to_owned(),
        o["fulfillments"][0]["id"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn labels_print_through_sendcloud() {
    let (client, t, log) = setup().await;
    let carriers = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/carriers",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(carriers[0]["name"], "sendcloud");

    for bad in [
        json!({ "provider": "sendcloud", "name": "S", "credentials": {},
                "settings": { "shipping_method_id": 8 } }),
        json!({ "provider": "sendcloud", "name": "S",
                "credentials": { "public_key": "p", "secret_key": "s" } }),
        json!({ "provider": "pigeon", "name": "S", "credentials": {} }),
    ] {
        send(
            &client,
            &t,
            "acme",
            "POST",
            "/api/carrier-accounts",
            bad,
            422,
        )
        .await;
    }
    let account = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/carrier-accounts",
        json!({ "provider": "sendcloud", "name": "Sendcloud DHL",
                "credentials": { "public_key": "pk", "secret_key": "sendcloud-secret" },
                "settings": { "shipping_method_id": 8 } }),
        201,
    )
    .await;
    assert!(account.get("credentials").is_none());
    let account = account["id"].as_str().unwrap().to_owned();
    let listed = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/carrier-accounts",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(listed[0]["name"], "Sendcloud DHL");
    assert!(!listed.to_string().contains("sendcloud-secret"));

    let (_, ship) = shipment(&client, &t, "acme", "10115").await;
    let label = send(
        &client,
        &t,
        "acme",
        "POST",
        &format!("/api/fulfillments/{ship}/labels"),
        json!({ "carrier_account_id": account }),
        201,
    )
    .await;
    assert_eq!(label["carrier"], "dhl");
    assert_eq!(label["tracking_number"], "JVGL0123");
    assert_eq!(label["status"], "active");
    assert_eq!(label["file"]["content_type"], "application/pdf");
    assert_eq!(label["file"]["file_name"], "label-1001.pdf");

    // Sendcloud saw the address, the weight and the keys.
    let sent = log.lock().unwrap().clone();
    let parcel = sent[0].json();
    assert_eq!(parcel["parcel"]["weight"], "1.000");
    assert_eq!(parcel["parcel"]["order_number"], "1001");
    assert_eq!(parcel["parcel"]["shipment"]["id"], 8);
    assert_eq!(
        sent[0].header("authorization"),
        Some("Basic cGs6c2VuZGNsb3VkLXNlY3JldA==")
    );

    // The shipment carries the tracking now.
    let labels = send(
        &client,
        &t,
        "acme",
        "GET",
        &format!("/api/fulfillments/{ship}/labels"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(labels.as_array().unwrap().len(), 1);
    let id = label["id"].as_str().unwrap().to_owned();
    let canceled = send(
        &client,
        &t,
        "acme",
        "POST",
        &format!("/api/labels/{id}/cancel"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(canceled["status"], "canceled");
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.path == "/parcels/4711/cancel"));
    send(
        &client,
        &t,
        "acme",
        "POST",
        &format!("/api/labels/{id}/cancel"),
        Value::Null,
        409,
    )
    .await;

    // Sendcloud's refusal is explained.
    let (_, bad) = shipment(&client, &t, "acme", "00000").await;
    let res = client
        .post(&format!("/api/fulfillments/{bad}/labels"))
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "carrier_account_id": account }))
        .send()
        .await;
    let res = res.assert_status(422);
    assert!(res.text().contains("Postal code is invalid"));

    // Another shop cannot use the account, nor see the label.
    let (_, theirs) = shipment(&client, &t, "globex", "10115").await;
    send(
        &client,
        &t,
        "globex",
        "POST",
        &format!("/api/fulfillments/{theirs}/labels"),
        json!({ "carrier_account_id": account }),
        422,
    )
    .await;
    send(
        &client,
        &t,
        "globex",
        "GET",
        &format!("/api/carrier-accounts/{account}"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        &t,
        "globex",
        "POST",
        &format!("/api/labels/{id}/cancel"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        &t,
        "globex",
        "GET",
        &format!("/api/fulfillments/{ship}/labels"),
        Value::Null,
        404,
    )
    .await;

    // Staff print one from the order page.
    let (order, ship) = shipment(&client, &t, "acme", "20095").await;
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    res.assert_status(303);
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let page = format!("/admin/acme/orders/{order}");
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Print label"));
    assert!(html.contains("Sendcloud DHL"));
    client
        .post(&format!("{page}/shipments/{ship}/label"))
        .header("cookie", &cookie)
        .form(&json!({ "carrier_account_id": account }))
        .send()
        .await
        .assert_status(303);
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Label (dhl)"));
    assert!(html.contains("JVGL0123"));
}
