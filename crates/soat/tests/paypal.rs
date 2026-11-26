//! C18: PayPal against a local fake of its API.

#![cfg(feature = "paypal")]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use soat_paypal::PayPalConfig;
use soat_testing::{fake_api, Log, Received};
use zapline::db::Database;
use zapline::test::TestClient;

/// Orders the fake holds, by id.
type Store = Arc<Mutex<HashMap<String, Value>>>;

/// `cid:secret`, base64.
const BASIC: &str = "Basic Y2lkOnNlY3JldA==";

fn answer(store: &Store, r: &Received) -> (u16, Value) {
    let parts: Vec<&str> = r.path.trim_start_matches('/').split('/').collect();
    let mut store = store.lock().unwrap();
    let find_auth = |store: &HashMap<String, Value>, auth: &str| {
        store
            .iter()
            .find(|(_, o)| o["purchase_units"][0]["payments"]["authorizations"][0]["id"] == auth)
            .map(|(k, _)| k.clone())
    };
    match (r.method.as_str(), parts.as_slice()) {
        ("POST", ["v1", "oauth2", "token"]) => {
            if r.header("authorization") != Some(BASIC) {
                return (
                    401,
                    json!({ "error_description": "Client Authentication failed" }),
                );
            }
            (200, json!({ "access_token": "A21", "expires_in": 32400 }))
        }
        (_, _) if r.header("authorization") != Some("Bearer A21") => {
            (401, json!({ "message": "no token" }))
        }
        ("POST", ["v2", "checkout", "orders"]) => {
            let body = r.json();
            let unit = &body["purchase_units"][0];
            if unit["amount"]["value"] == "0.13" {
                return (
                    422,
                    json!({ "details": [{ "description": "Payee account is restricted." }] }),
                );
            }
            let id: String = format!("O{}", r.header("paypal-request-id").unwrap_or("x"))
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect();
            let o = json!({
                "id": id, "intent": body["intent"], "status": "PAYER_ACTION_REQUIRED",
                "purchase_units": [{ "reference_id": unit["reference_id"], "custom_id": unit["custom_id"],
                                     "amount": unit["amount"] }],
                "links": [{ "rel": "payer-action", "href": format!("https://www.sandbox.paypal.com/checkoutnow?token={id}") }],
            });
            store.insert(id, o.clone());
            (200, o)
        }
        ("GET", ["v2", "checkout", "orders", id]) => match store.get(*id) {
            Some(o) => (200, o.clone()),
            None => (
                404,
                json!({ "details": [{ "description": "order not found" }] }),
            ),
        },
        ("POST", ["v2", "checkout", "orders", id, step]) => {
            let o = store.get_mut(*id).unwrap();
            o["status"] = json!("COMPLETED");
            o["purchase_units"][0]["payments"] = if *step == "capture" {
                json!({ "captures": [{ "id": format!("CAP{id}"), "status": "COMPLETED" }] })
            } else {
                json!({ "authorizations": [{ "id": format!("AUTH{id}"), "status": "CREATED" }] })
            };
            (201, o.clone())
        }
        ("POST", ["v2", "payments", "authorizations", auth, step]) => {
            let Some(key) = find_auth(&store, auth) else {
                return (404, json!({ "message": "no such authorization" }));
            };
            let o = store.get_mut(&key).unwrap();
            let payments = &mut o["purchase_units"][0]["payments"];
            if *step == "capture" {
                payments["authorizations"][0]["status"] = json!("CAPTURED");
                payments["captures"] =
                    json!([{ "id": format!("CAP{auth}"), "status": "COMPLETED" }]);
                (
                    201,
                    json!({ "id": format!("CAP{auth}"), "status": "COMPLETED" }),
                )
            } else {
                payments["authorizations"][0]["status"] = json!("VOIDED");
                (204, Value::Null)
            }
        }
        ("POST", ["v2", "payments", "captures", _, "refund"]) => {
            (201, json!({ "id": "REF1", "status": "COMPLETED" }))
        }
        ("POST", ["v1", "notifications", "verify-webhook-signature"]) => {
            let ok = r.json()["transmission_sig"] == "good" && r.json()["webhook_id"] == "WH1";
            (
                200,
                json!({ "verification_status": if ok { "SUCCESS" } else { "FAILURE" } }),
            )
        }
        _ => (404, json!({ "message": "no such route" })),
    }
}

async fn app() -> (TestClient, String, Store, Log) {
    let store: Store = Arc::default();
    let s = store.clone();
    let (base, log) = fake_api(move |r| answer(&s, r)).await;
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.paypal = Some(PayPalConfig {
        client_id: "cid".into(),
        client_secret: "secret".into(),
        webhook_id: "WH1".into(),
        api_base: base,
    });
    let client = soat::app(Database::postgres(url), settings).test();
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, &["acme"]).await;
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

async fn method(client: &TestClient, t: &str, settings: Value) -> String {
    let m = send(
        client,
        t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "paypal", "name": "PayPal", "settings": settings }),
        201,
    )
    .await;
    m["id"].as_str().unwrap().to_owned()
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

/// PayPal's event that the buyer approved `order`.
async fn approved(client: &TestClient, store: &Store, order: &str, signature: &str) -> u16 {
    store.lock().unwrap().get_mut(order).unwrap()["status"] = json!("APPROVED");
    let event = json!({ "id": "WH-1", "event_type": "CHECKOUT.ORDER.APPROVED",
                        "resource": { "id": order, "status": "APPROVED" } });
    client
        .post("/api/payments/paypal/webhook")
        .header("paypal-transmission-id", "t1")
        .header("paypal-transmission-sig", signature)
        .header("paypal-transmission-time", "2026-11-23T10:00:00Z")
        .header("paypal-auth-algo", "SHA256withRSA")
        .header("paypal-cert-url", "https://api.paypal.com/cert")
        .json(&event)
        .send()
        .await
        .status()
        .as_u16()
}

fn calls(log: &Log, path: &str) -> Vec<Received> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|r| r.path == path)
        .cloned()
        .collect()
}

#[tokio::test]
async fn buyers_approve_at_paypal_and_the_server_captures() {
    let (client, t, store, log) = app().await;
    for bad in [json!({ "client_secret": "x" }), json!({ "intent": "sale" })] {
        send(
            &client,
            &t,
            "POST",
            "/api/payment-methods",
            json!({ "provider": "paypal", "name": "PayPal", "settings": bad }),
            422,
        )
        .await;
    }
    let m = method(&client, &t, json!({})).await;
    let p = pay(&client, &t, &m, 1999).await;
    let id = p["id"].as_str().unwrap();
    let order = p["provider_ref"].as_str().unwrap().to_owned();
    assert_eq!(p["status"], "requires_action");
    assert_eq!(
        p["next_action"]["url"],
        format!("https://www.sandbox.paypal.com/checkoutnow?token={order}")
    );
    let created = &calls(&log, "/v2/checkout/orders")[0];
    assert_eq!(created.header("paypal-request-id"), Some(id));
    let body = created.json();
    assert_eq!(body["intent"], "CAPTURE");
    assert_eq!(body["purchase_units"][0]["custom_id"], format!("acme:{id}"));
    assert_eq!(
        body["purchase_units"][0]["amount"],
        json!({ "currency_code": "EUR", "value": "19.99" })
    );
    assert_eq!(
        body["payment_source"]["paypal"]["experience_context"]["return_url"],
        "https://shop.example/thanks"
    );

    // Unverified events are refused and change nothing.
    assert_eq!(approved(&client, &store, &order, "forged").await, 400);
    let p = send(
        &client,
        &t,
        "GET",
        &format!("/api/payments/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "requires_action");

    assert_eq!(approved(&client, &store, &order, "good").await, 200);
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
    assert_eq!(
        calls(&log, &format!("/v2/checkout/orders/{order}/capture")).len(),
        1
    );

    send(
        &client,
        &t,
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({ "amount_minor": 500 }),
        201,
    )
    .await;
    let refund = &calls(&log, &format!("/v2/payments/captures/CAP{order}/refund"))[0];
    assert_eq!(refund.json()["amount"]["value"], "5.00");
    // One token for all of it.
    assert_eq!(calls(&log, "/v1/oauth2/token").len(), 1);
}

#[tokio::test]
async fn authorize_then_capture_or_void() {
    let (client, t, store, log) = app().await;
    let m = method(
        &client,
        &t,
        json!({ "intent": "authorize", "merchant_id": "MERCHANT123" }),
    )
    .await;

    let p = pay(&client, &t, &m, 1000).await;
    let id = p["id"].as_str().unwrap();
    let order = p["provider_ref"].as_str().unwrap().to_owned();
    let created = calls(&log, "/v2/checkout/orders")[0].json();
    assert_eq!(created["intent"], "AUTHORIZE");
    assert_eq!(
        created["purchase_units"][0]["payee"]["merchant_id"],
        "MERCHANT123"
    );
    assert_eq!(approved(&client, &store, &order, "good").await, 200);
    let p = send(
        &client,
        &t,
        "GET",
        &format!("/api/payments/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "authorized");
    let p = send(
        &client,
        &t,
        "POST",
        &format!("/api/payments/{id}/capture"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "paid");
    let capture = &calls(
        &log,
        &format!("/v2/payments/authorizations/AUTH{order}/capture"),
    )[0];
    assert!(capture
        .header("paypal-auth-assertion")
        .unwrap()
        .starts_with("eyJhbGciOiJub25lIn0."));

    let p = pay(&client, &t, &m, 1000).await;
    let id = p["id"].as_str().unwrap();
    let order = p["provider_ref"].as_str().unwrap().to_owned();
    assert_eq!(approved(&client, &store, &order, "good").await, 200);
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
    assert_eq!(
        calls(
            &log,
            &format!("/v2/payments/authorizations/AUTH{order}/void")
        )
        .len(),
        1
    );

    // A refusal fails the payment with PayPal's reason.
    let p = pay(&client, &t, &m, 13).await;
    assert_eq!(p["status"], "failed");
    assert_eq!(p["failure_reason"], "PayPal: Payee account is restricted.");
}
