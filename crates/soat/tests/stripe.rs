//! C8: Stripe against a local fake of its API.

#![cfg(feature = "stripe")]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use soat_stripe::signature::sign;
use soat_stripe::StripeConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zapline::db::Database;
use zapline::test::TestClient;

/// A request the fake received: path, lowercased headers, body.
type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>, String)>>>;

const WEBHOOK_SECRET: &str = "whsec_test";

/// The fake names each intent after the idempotency key (the payment id).
fn answer(path: &str, key: &str, body: &str) -> (u16, Value) {
    if body.contains("amount=13&") {
        return (
            402,
            json!({ "error": { "message": "Your card was declined." } }),
        );
    }
    let pi =
        |id: &str, status: &str| json!({ "id": id, "object": "payment_intent", "status": status });
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["v1", "payment_intents"] => {
            let id = format!("pi_{key}");
            let mut v = pi(&id, "requires_payment_method");
            v["client_secret"] = json!(format!("{id}_secret_x"));
            (200, v)
        }
        ["v1", "payment_intents", id, "capture"] => (200, pi(id, "succeeded")),
        ["v1", "payment_intents", id, "cancel"] => (200, pi(id, "canceled")),
        ["v1", "refunds"] => (200, json!({ "id": "re_1", "object": "refund" })),
        _ => (404, json!({ "error": { "message": "no such route" } })),
    }
}

/// A fake Stripe API on a local port.
async fn fake_stripe() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (path, headers, body) = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let head = &text[..end];
                    let headers: Vec<(String, String)> = head
                        .lines()
                        .skip(1)
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                        .collect();
                    let len = headers
                        .iter()
                        .find(|(k, _)| k == "content-length")
                        .map_or(0, |(_, v)| v.parse::<usize>().unwrap());
                    if buf.len() >= end + 4 + len {
                        let path = head.split(' ').nth(1).unwrap().to_owned();
                        let body =
                            String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string();
                        break (path, headers, body);
                    }
                };
                let key = headers
                    .iter()
                    .find(|(k, _)| k == "idempotency-key")
                    .map_or(String::new(), |(_, v)| v.clone());
                let (status, reply) = answer(&path, &key, &format!("{body}&"));
                log.lock().unwrap().push((path, headers, body));
                let reply = reply.to_string();
                let out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(out.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

async fn app() -> (TestClient, String, Seen) {
    let (base, seen) = fake_stripe().await;
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.stripe = Some(StripeConfig {
        secret_key: "sk_test_platform".into(),
        webhook_secret: WEBHOOK_SECRET.into(),
        api_base: base,
    });
    let client = soat::app(Database::postgres(url), settings).test();
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, &["acme"]).await;
    (client, token, seen)
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
        json!({ "provider": "stripe", "name": "Card", "settings": settings }),
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
        json!({ "method_id": method, "reference": "ord_1", "amount_minor": amount, "currency": "EUR" }),
        201,
    )
    .await
}

async fn event(client: &TestClient, kind: &str, object: Value, secret: &str) -> u16 {
    let body = json!({ "id": "evt_1", "type": kind, "data": { "object": object } }).to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    client
        .post("/api/payments/stripe/webhook")
        .header("stripe-signature", &sign(secret, now, body.as_bytes()))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .status()
        .as_u16()
}

fn intent(status: &str, payment: &str, tenant: &str) -> Value {
    json!({ "id": format!("pi_{payment}"), "object": "payment_intent", "status": status,
            "metadata": { "soat_tenant": tenant, "soat_payment": payment } })
}

#[tokio::test]
async fn card_payments_start_with_stripe_and_follow_its_events() {
    let (client, t, seen) = app().await;
    let m = method(
        &client,
        &t,
        json!({ "account_id": "acct_123", "publishable_key": "pk_test_1" }),
    )
    .await;
    let p = pay(&client, &t, &m, 2500).await;
    let id = p["id"].as_str().unwrap().to_owned();
    assert_eq!(p["status"], "requires_action");
    assert_eq!(p["provider_ref"], format!("pi_{id}"));
    assert_eq!(p["client_secret"], format!("pi_{id}_secret_x"));
    assert_eq!(p["next_action"]["publishable_key"], "pk_test_1");
    {
        let seen = seen.lock().unwrap();
        let (path, headers, body) = &seen[0];
        assert_eq!(path, "/v1/payment_intents");
        let h = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(h("authorization"), Some("Bearer sk_test_platform"));
        assert_eq!(h("stripe-account"), Some("acct_123"));
        assert_eq!(h("idempotency-key"), Some(id.as_str()));
        assert!(body.contains("amount=2500"));
        assert!(body.contains("currency=eur"));
        assert!(body.contains(&format!("metadata%5Bsoat_payment%5D={id}")));
    }
    // Never stored: reading it back has no client secret.
    let again = send(
        &client,
        &t,
        "GET",
        &format!("/api/payments/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert!(again.get("client_secret").is_none());

    // Unsigned, wrongly signed, or for another account's intent: no change.
    assert_eq!(
        event(
            &client,
            "payment_intent.succeeded",
            intent("succeeded", &id, "acme"),
            "whsec_wrong"
        )
        .await,
        400
    );
    let mut other = intent("succeeded", &id, "acme");
    other["id"] = json!("pi_evil");
    assert_eq!(
        event(&client, "payment_intent.succeeded", other, WEBHOOK_SECRET).await,
        204
    );
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

    assert_eq!(
        event(
            &client,
            "payment_intent.succeeded",
            intent("succeeded", &id, "acme"),
            WEBHOOK_SECRET
        )
        .await,
        204
    );
    // Repeated and late events are harmless.
    assert_eq!(
        event(
            &client,
            "payment_intent.succeeded",
            intent("succeeded", &id, "acme"),
            WEBHOOK_SECRET
        )
        .await,
        204
    );
    assert_eq!(
        event(
            &client,
            "payment_intent.processing",
            intent("processing", &id, "acme"),
            WEBHOOK_SECRET
        )
        .await,
        204
    );
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

    let r = send(
        &client,
        &t,
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({ "amount_minor": 500 }),
        201,
    )
    .await;
    assert_eq!(r["refund"]["provider_ref"], "re_1");
    let seen = seen.lock().unwrap();
    let (path, _, body) = seen.last().unwrap();
    assert_eq!(path, "/v1/refunds");
    assert!(body.contains(&format!("payment_intent=pi_{id}")));
    assert!(body.contains("amount=500"));
}

#[tokio::test]
async fn manual_capture_cancel_and_declines() {
    let (client, t, _) = app().await;
    let m = method(&client, &t, json!({ "capture": "manual" })).await;
    let p = pay(&client, &t, &m, 2500).await;
    let id = p["id"].as_str().unwrap().to_owned();
    event(
        &client,
        "payment_intent.amount_capturable_updated",
        intent("requires_capture", &id, "acme"),
        WEBHOOK_SECRET,
    )
    .await;
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

    let p = pay(&client, &t, &m, 700).await;
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

    // Stripe refuses: the payment is failed with its reason.
    let p = pay(&client, &t, &m, 13).await;
    assert_eq!(p["status"], "failed");
    assert_eq!(p["failure_reason"], "Stripe: Your card was declined.");
    assert!(p.get("client_secret").is_none());

    for bad in [
        json!({ "secret_key": "sk_live_x" }),
        json!({ "capture": "later" }),
    ] {
        send(
            &client,
            &t,
            "POST",
            "/api/payment-methods",
            json!({ "provider": "stripe", "name": "Bad", "settings": bad }),
            422,
        )
        .await;
    }
}
