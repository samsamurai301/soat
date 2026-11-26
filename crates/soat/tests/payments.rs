//! C7: payment methods, payments, refunds and the manual provider.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    shop: &str,
    token: Option<&str>,
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
    .header("x-soat-tenant", shop);
    let req = match token {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 || status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

async fn bank_transfer(client: &TestClient, t: &str) -> String {
    let m = send(
        client,
        "acme",
        Some(t),
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Bank transfer",
                "settings": { "instructions": "IBAN DE00 1234" } }),
        201,
    )
    .await;
    m["id"].as_str().unwrap().to_owned()
}

async fn pay(client: &TestClient, t: &str, method: &str, amount: i64) -> Value {
    send(
        client,
        "acme",
        Some(t),
        "POST",
        "/api/payments",
        json!({ "method_id": method, "reference": "ord_1", "amount_minor": amount, "currency": "EUR" }),
        201,
    )
    .await
}

#[tokio::test]
async fn manual_payments_are_marked_paid_and_refunded() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let t = t.as_str();
    let providers = send(
        &client,
        "acme",
        Some(t),
        "GET",
        "/api/payment-providers",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(providers["items"][0]["name"], "manual");
    let method = bank_transfer(&client, t).await;
    assert!(method.starts_with("pm_"));
    let public = send(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/payment-methods",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(public["items"][0]["name"], "Bank transfer");
    assert!(public["items"][0].get("settings").is_none());

    let p = pay(&client, t, &method, 5000).await;
    let id = p["id"].as_str().unwrap();
    assert!(id.starts_with("pay_"));
    assert_eq!(p["status"], "pending");
    assert_eq!(p["next_action"]["text"], "IBAN DE00 1234");

    // Not collected yet: no refunds.
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({}),
        409,
    )
    .await;
    let p = send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/mark-paid"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "paid");
    assert!(p["next_action"].is_null());
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/mark-paid"),
        Value::Null,
        409,
    )
    .await;
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/cancel"),
        Value::Null,
        409,
    )
    .await;

    let r = send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({ "amount_minor": 2000, "reason": "damaged" }),
        201,
    )
    .await;
    assert!(r["refund"]["id"].as_str().unwrap().starts_with("rfd_"));
    assert_eq!(r["payment"]["status"], "partially_refunded");
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({ "amount_minor": 3001 }),
        422,
    )
    .await;
    let r = send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/refunds"),
        json!({}),
        201,
    )
    .await;
    assert_eq!(r["refund"]["amount_minor"], 3000);
    assert_eq!(r["payment"]["status"], "refunded");
    assert_eq!(r["payment"]["refunded_minor"], 5000);
    let list = send(
        &client,
        "acme",
        Some(t),
        "GET",
        &format!("/api/payments/{id}/refunds"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);

    let page = send(
        &client,
        "acme",
        Some(t),
        "GET",
        "/api/payments?reference=ord_1",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 1);
    let events = send(
        &client,
        "acme",
        Some(t),
        "GET",
        "/api/events?kind=payment.refunded",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 2);
}

#[tokio::test]
async fn methods_are_checked_and_cancellable_payments_cancel() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let t = t.as_str();
    for bad in [
        json!({ "provider": "nope", "name": "X" }),
        json!({ "provider": "manual", "name": "X", "settings": { "secret": "x" } }),
    ] {
        send(
            &client,
            "acme",
            Some(t),
            "POST",
            "/api/payment-methods",
            bad,
            422,
        )
        .await;
    }
    let method = bank_transfer(&client, t).await;
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Bank transfer" }),
        409,
    )
    .await;
    let p = pay(&client, t, &method, 100).await;
    let id = p["id"].as_str().unwrap();
    let p = send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/cancel"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["status"], "canceled");
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/mark-paid"),
        Value::Null,
        409,
    )
    .await;
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        &format!("/api/payments/{id}/capture"),
        Value::Null,
        409,
    )
    .await;

    // Inactive methods take no payments and are hidden from buyers.
    send(
        &client,
        "acme",
        Some(t),
        "PATCH",
        &format!("/api/payment-methods/{method}"),
        json!({ "active": false }),
        200,
    )
    .await;
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        "/api/payments",
        json!({ "method_id": method, "reference": "r", "amount_minor": 1, "currency": "EUR" }),
        422,
    )
    .await;
    let public = send(
        &client,
        "acme",
        None,
        "GET",
        "/api/storefront/payment-methods",
        Value::Null,
        200,
    )
    .await;
    assert!(public["items"].as_array().unwrap().is_empty());
    send(
        &client,
        "acme",
        Some(t),
        "POST",
        "/api/payments",
        json!({ "method_id": method, "reference": "r", "amount_minor": 0, "currency": "EUR" }),
        422,
    )
    .await;
    send(
        &client,
        "acme",
        Some(t),
        "DELETE",
        &format!("/api/payment-methods/{method}"),
        Value::Null,
        204,
    )
    .await;
    let p = send(
        &client,
        "acme",
        Some(t),
        "GET",
        &format!("/api/payments/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert!(p["method_id"].is_null());
    assert_eq!(p["provider"], "manual");
}

#[tokio::test]
async fn another_shops_payments_are_not_found() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let t = t.as_str();
    let method = bank_transfer(&client, t).await;
    let p = pay(&client, t, &method, 100).await;
    let id = p["id"].as_str().unwrap();
    for (m, path) in [
        ("GET", format!("/api/payments/{id}")),
        ("POST", format!("/api/payments/{id}/mark-paid")),
        ("POST", format!("/api/payments/{id}/cancel")),
        ("GET", format!("/api/payments/{id}/refunds")),
        ("DELETE", format!("/api/payment-methods/{method}")),
    ] {
        send(&client, "globex", Some(t), m, &path, Value::Null, 404).await;
    }
    send(
        &client,
        "globex",
        Some(t),
        "POST",
        "/api/payments",
        json!({ "method_id": method, "reference": "r", "amount_minor": 1, "currency": "EUR" }),
        422,
    )
    .await;
    let page = send(
        &client,
        "globex",
        Some(t),
        "GET",
        "/api/payments",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 0);
}
