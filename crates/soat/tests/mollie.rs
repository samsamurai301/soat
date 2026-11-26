//! C18: Mollie against a local fake of its API.

#![cfg(feature = "mollie")]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use soat_mollie::MollieConfig;
use soat_testing::{fake_api, Log, Received};
use zapline::db::Database;
use zapline::test::TestClient;

/// Payments the fake holds, by id.
type Store = Arc<Mutex<HashMap<String, Value>>>;

fn answer(store: &Store, r: &Received) -> (u16, Value) {
    let parts: Vec<&str> = r
        .path
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_start_matches('/')
        .split('/')
        .collect();
    let mut store = store.lock().unwrap();
    match (r.method.as_str(), parts.as_slice()) {
        ("POST", ["v2", "payments"]) => {
            let body = r.json();
            if body["amount"]["value"] == "0.13" {
                return (
                    422,
                    json!({ "status": 422, "detail": "The amount is lower than the minimum." }),
                );
            }
            let id = format!("tr_{}", r.header("idempotency-key").unwrap_or("x"));
            let p = json!({
                "id": id, "status": "open", "metadata": body["metadata"],
                "_links": { "checkout": { "href": format!("https://www.mollie.com/checkout/{id}") } },
            });
            store.insert(id, p.clone());
            (201, p)
        }
        ("GET", ["v2", "payments", id]) => match store.get(*id) {
            Some(p) => (200, p.clone()),
            None => (
                404,
                json!({ "status": 404, "detail": "No payment exists with token" }),
            ),
        },
        ("DELETE", ["v2", "payments", id]) => {
            let p = store.get_mut(*id).unwrap();
            p["status"] = json!("canceled");
            (200, p.clone())
        }
        ("POST", ["v2", "payments", id, "captures"]) => {
            store.get_mut(*id).unwrap()["status"] = json!("paid");
            (201, json!({ "id": "cpt_1" }))
        }
        ("POST", ["v2", "payments", _, "refunds"]) => (201, json!({ "id": "re_1" })),
        _ => (404, json!({ "status": 404, "detail": "no such route" })),
    }
}

async fn app() -> (TestClient, String, Store, Log) {
    let store: Store = Arc::default();
    let s = store.clone();
    let (base, log) = fake_api(move |r| answer(&s, r)).await;
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.mollie = Some(MollieConfig {
        api_key: "test_platform".into(),
        api_base: base,
        public_url: "https://shop.example/".into(),
        testmode: false,
    });
    let client = soat::app(Database::postgres(url), settings).test();
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, &["acme", "other"]).await;
    (client, token, store, log)
}

async fn send(
    client: &TestClient,
    t: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
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

async fn pay(client: &TestClient, t: &str, method: &str, amount: i64) -> Value {
    send(
        client,
        t,
        "POST",
        "/api/payments",
        json!({ "method_id": method, "reference": "ord_1", "amount_minor": amount, "currency": "EUR",
                "return_url": "https://shop.example/thanks" }),
        201,
    )
    .await
}

async fn webhook(client: &TestClient, body: &str) -> u16 {
    client
        .post("/api/payments/mollie/webhook")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.to_owned())
        .send()
        .await
        .status()
        .as_u16()
}

#[tokio::test]
async fn payments_redirect_to_mollie_and_follow_its_webhooks() {
    let (client, t, store, log) = app().await;
    for bad in [
        json!({ "api_key": "live_x" }),
        json!({ "method": "iDEAL!" }),
    ] {
        send(
            &client,
            &t,
            "POST",
            "/api/payment-methods",
            json!({ "provider": "mollie", "name": "iDEAL", "settings": bad }),
            422,
        )
        .await;
    }
    let m = send(
        &client,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "mollie", "name": "iDEAL", "settings": { "method": "ideal" } }),
        201,
    )
    .await;
    let m = m["id"].as_str().unwrap();

    let p = pay(&client, &t, m, 1999).await;
    let id = p["id"].as_str().unwrap();
    let tr = format!("tr_{id}");
    assert_eq!(p["status"], "requires_action");
    assert_eq!(p["provider_ref"], tr);
    assert_eq!(p["next_action"]["type"], "redirect");
    assert_eq!(
        p["next_action"]["url"],
        format!("https://www.mollie.com/checkout/{tr}")
    );
    let sent = log.lock().unwrap()[0].clone();
    assert_eq!(sent.header("authorization"), Some("Bearer test_platform"));
    assert_eq!(sent.header("idempotency-key"), Some(id));
    let body = sent.json();
    assert_eq!(
        body["amount"],
        json!({ "currency": "EUR", "value": "19.99" })
    );
    assert_eq!(body["method"], "ideal");
    assert_eq!(body["redirectUrl"], "https://shop.example/thanks");
    assert_eq!(
        body["webhookUrl"],
        "https://shop.example/api/payments/mollie/webhook"
    );
    assert_eq!(body["metadata"]["soat_tenant"], "acme");
    assert_eq!(body["metadata"]["soat_payment"], id);

    // Mollie says only which payment changed; the server asks Mollie.
    store.lock().unwrap().get_mut(&tr).unwrap()["status"] = json!("paid");
    assert_eq!(webhook(&client, &format!("id={tr}")).await, 200);
    let p = send(
        &client,
        &t,
        "GET",
        &format!("/api/payments/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "paid");
    assert!(p["next_action"].is_null());
    let last = log.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        (last.method.as_str(), last.path.as_str()),
        ("GET", &*format!("/v2/payments/{tr}"))
    );

    // Unknown ids and junk are acknowledged and change nothing.
    assert_eq!(webhook(&client, "id=tr_unknown").await, 200);
    assert_eq!(webhook(&client, "id=../../v2/x").await, 200);
    assert_eq!(webhook(&client, "nothing").await, 200);

    send(
        &client,
        &t,
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({ "amount_minor": 500 }),
        201,
    )
    .await;
    let last = log.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.path, format!("/v2/payments/{tr}/refunds"));
    assert_eq!(last.json()["amount"]["value"], "5.00");
}

#[tokio::test]
async fn cancels_declines_and_foreign_payments() {
    let (client, t, store, log) = app().await;
    let m = send(
        &client,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "mollie", "name": "Mollie", "settings": {} }),
        201,
    )
    .await;
    let m = m["id"].as_str().unwrap();

    // Without a return URL Mollie cannot send the buyer back; a refusal
    // fails the payment with Mollie's reason.
    let p = send(
        &client,
        &t,
        "POST",
        "/api/payments",
        json!({ "method_id": m, "reference": "ord_1", "amount_minor": 100, "currency": "EUR" }),
        201,
    )
    .await;
    assert_eq!(p["status"], "failed");
    assert_eq!(
        p["failure_reason"],
        "Mollie sends the buyer back: give a return_url"
    );
    let p = pay(&client, &t, m, 13).await;
    assert_eq!(p["status"], "failed");
    assert_eq!(
        p["failure_reason"],
        "Mollie: The amount is lower than the minimum."
    );

    let p = pay(&client, &t, m, 1000).await;
    let id = p["id"].as_str().unwrap();
    let p = send(
        &client,
        &t,
        "POST",
        &format!("/api/payments/{id}/cancel"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "canceled");
    let last = log.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.method, "DELETE");

    // A Mollie payment whose metadata names another payment moves nothing.
    let other = pay(&client, &t, m, 1000).await;
    let other_id = other["id"].as_str().unwrap();
    store.lock().unwrap().insert(
        "tr_forged".into(),
        json!({ "id": "tr_forged", "status": "paid",
                "metadata": { "soat_tenant": "acme", "soat_payment": other_id } }),
    );
    assert_eq!(webhook(&client, "id=tr_forged").await, 200);
    let other = send(
        &client,
        &t,
        "GET",
        &format!("/api/payments/{other_id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(other["status"], "requires_action");
}
