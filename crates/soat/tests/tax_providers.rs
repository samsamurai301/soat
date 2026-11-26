//! C5: a shop's tax worked out by a tax service instead of its zones.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use soat_testing::fake_api;
use zapline::test::TestClient;

const SECRET: &str = "the tax service's shared secret";

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
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn question(postal_code: &str) -> Value {
    json!({ "country": "US", "postal_code": postal_code, "lines": [
        { "tax_class": "standard", "amount_minor": 10000 },
        { "tax_class": "reduced", "amount_minor": 2000 }
    ] })
}

#[tokio::test]
async fn a_tax_service_works_out_the_shops_tax() {
    let down = Arc::new(AtomicBool::new(false));
    let flag = down.clone();
    let (base, log) = fake_api(move |r| {
        if flag.load(Ordering::SeqCst) {
            return (500, json!({ "error": "down" }));
        }
        let signed = r
            .header("soat-signature")
            .is_some_and(|s| soat_webhooks::verify(SECRET, s, r.body.as_bytes()));
        if !signed {
            return (401, json!({ "error": "unsigned" }));
        }
        let asked: Value = serde_json::from_str(&r.body).unwrap();
        // 8.875% in New York, 6% elsewhere, on every line.
        let bps = if asked["postal_code"] == "10001" {
            888
        } else {
            600
        };
        let lines: Vec<Value> = asked["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let amount = l["amount_minor"].as_i64().unwrap();
                json!({ "rate_bps": bps, "tax_minor": amount * bps / 10000 })
            })
            .collect();
        (200, json!({ "lines": lines }))
    })
    .await;
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/settings",
        json!({ "origin_country": "US", "prices_include_tax": false,
                "charge_destination_vat": false }),
        200,
    )
    .await;

    // Without a provider, the zones work it out (no US zone: no tax).
    let q = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("10001"),
        200,
    )
    .await;
    assert_eq!(q["source"], "zones");
    assert_eq!(q["tax_minor"], 0);
    send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/tax/provider",
        Value::Null,
        404,
    )
    .await;
    let kinds = send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/tax/providers",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(kinds[0]["name"], "http");

    // Picking one checks it, and never shows the secret again.
    let url = format!("{base}/quote");
    for (bad, code) in [
        (
            json!({ "provider": "avalara", "credentials": {} }),
            "unknown_provider",
        ),
        (
            json!({ "provider": "http", "settings": { "url": "http://tax.example.com" },
                    "credentials": { "secret": SECRET } }),
            "invalid_settings",
        ),
        (
            json!({ "provider": "http", "settings": { "url": url },
                    "credentials": { "secret": "short" } }),
            "invalid_credentials",
        ),
    ] {
        let e = send(&client, "acme", &t, "PUT", "/api/tax/provider", bad, 422).await;
        assert_eq!(e["code"], code);
    }
    let set = send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/provider",
        json!({ "provider": "http", "settings": { "url": url },
                "credentials": { "secret": SECRET } }),
        200,
    )
    .await;
    assert_eq!(set["provider"], "http");
    assert!(!set.to_string().contains(SECRET));

    // The service's answer is the shop's tax, kept a few minutes.
    let q = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("10001"),
        200,
    )
    .await;
    assert_eq!(q["source"], "http", "{q}");
    assert_eq!(q["lines"][0]["rate_bps"], 888);
    assert_eq!(q["tax_minor"], 888 + 177);
    assert!(q["provider_error"].is_null());
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("10001"),
        200,
    )
    .await;
    let calls = || log.lock().unwrap().len();
    assert_eq!(calls(), 1);
    let q = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("90210"),
        200,
    )
    .await;
    assert_eq!(q["tax_minor"], 600 + 120);
    assert_eq!(calls(), 2);
    let asked: Value = serde_json::from_str(&log.lock().unwrap()[1].body).unwrap();
    assert_eq!(asked["origin_country"], "US");
    assert_eq!(asked["lines"][1]["tax_class"], "reduced");

    // When it is down, the zones stand in and the quote says why.
    down.store(true, Ordering::SeqCst);
    let q = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("60601"),
        200,
    )
    .await;
    assert_eq!(q["source"], "zones");
    assert!(q["provider_error"].as_str().unwrap().contains("500"), "{q}");

    // Another shop has none of it.
    send(
        &client,
        "globex",
        &t,
        "GET",
        "/api/tax/provider",
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "globex",
        &t,
        "DELETE",
        "/api/tax/provider",
        Value::Null,
        404,
    )
    .await;
    let q = send(
        &client,
        "globex",
        &t,
        "POST",
        "/api/tax/quote",
        question("10001"),
        200,
    )
    .await;
    assert_eq!(q["source"], "zones");

    // Removing it goes back to the zones; the secret never reached the log.
    send(
        &client,
        "acme",
        &t,
        "DELETE",
        "/api/tax/provider",
        Value::Null,
        204,
    )
    .await;
    down.store(false, Ordering::SeqCst);
    let q = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        question("10001"),
        200,
    )
    .await;
    assert_eq!(q["source"], "zones");
    let events = send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/events?limit=50",
        Value::Null,
        200,
    )
    .await;
    let text = events.to_string();
    assert!(text.contains("tax.provider_set") && text.contains("tax.provider_removed"));
    assert!(!text.contains(SECRET));
}

#[tokio::test]
async fn checkout_charges_the_services_tax() {
    let (base, _log) = fake_api(|r| {
        let asked: Value = serde_json::from_str(&r.body).unwrap();
        let lines: Vec<Value> = asked["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let amount = l["amount_minor"].as_i64().unwrap();
                json!({ "rate_bps": 1000, "tax_minor": amount / 10 })
            })
            .collect();
        (200, json!({ "lines": lines }))
    })
    .await;
    let (client, t) = common::with_tenants(&["acme"]).await;
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/settings",
        json!({ "origin_country": "DE", "prices_include_tax": false,
                "charge_destination_vat": false }),
        200,
    )
    .await;
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/provider",
        json!({ "provider": "http", "settings": { "url": format!("{base}/q") },
                "credentials": { "secret": SECRET } }),
        200,
    )
    .await;
    let p = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 5000, "currency": "EUR" }),
        201,
    )
    .await;
    let draft = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/draft-orders",
        json!({ "currency": "EUR", "email": "ada@example.com",
                "lines": [{ "item_id": p["id"], "quantity": 1 }],
                "shipping_address": { "first_name": "Ada", "last_name": "L", "line1": "Main St 1",
                    "city": "Berlin", "postal_code": "10115", "country": "DE" } }),
        201,
    )
    .await;
    assert_eq!(draft["tax_minor"], 500, "{draft}");
}
