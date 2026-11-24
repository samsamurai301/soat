//! C12: returns and refunds.

mod common;

use serde_json::{json, Value};
use soat_core::Currency;
use soat_orders::{place, NewOrder, NewOrderLine, NewShipping};
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

async fn placed(url: &str, item: &str, customer: &str) -> String {
    let pool = PgPool::connect(url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let order = NewOrder {
        cart_id: None,
        customer_id: Some(customer.into()),
        email: "ada@example.com".into(),
        currency: Currency::parse("EUR").unwrap(),
        lines: vec![NewOrderLine {
            item_id: item.into(),
            product_id: Some(item.into()),
            sku: "TEE".into(),
            title: "Tee".into(),
            quantity: 2,
            unit_price_minor: 1190,
            tax_class: "standard".into(),
            tax_rate_bps: 1900,
            tax_minor: 380,
            discount_minor: 0,
            requires_shipping: true,
        }],
        shipping: Some(NewShipping {
            rate_id: "shr_x".into(),
            name: "Standard".into(),
            price_minor: 495,
            discount_minor: 0,
            tax_minor: 79,
            tax_rate_bps: 1900,
        }),
        shipping_address: None,
        billing_address: None,
        prices_include_tax: true,
        tax_country: Some("DE".into()),
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

#[tokio::test]
async fn returned_units_are_restocked_and_refunded() {
    let (client, url) = common::client_and_url().await;
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "globex"]).await;
    let t = t.as_str();
    let loc = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/locations",
        json!({ "name": "A", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    let tee = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 1190, "currency": "EUR" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/inventory/{tee}/adjustments"),
        json!({ "location_id": loc, "delta": 5, "reason": "received" }),
        200,
    )
    .await;
    let s = send(
        &client,
        "acme",
        "unused",
        "POST",
        "/api/storefront/account/register",
        json!({ "email": "ada@example.com", "password": "a long password" }),
        201,
    )
    .await;
    let customer = s["token"].as_str().unwrap().to_owned();
    let order = placed(&url, &tee, s["customer"]["id"].as_str().unwrap()).await;

    // Paid by invoice: 2 x 11.90 + 4.95 shipping, VAT included.
    let m = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice" }),
        201,
    )
    .await;
    let p = send(
        &client,
        "acme",
        t,
        "POST",
        "/api/payments",
        json!({ "method_id": m["id"], "reference": order, "amount_minor": 2875, "currency": "EUR" }),
        201,
    )
    .await;
    let pay = p["id"].as_str().unwrap().to_owned();
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/payments/{pay}/mark-paid"),
        Value::Null,
        200,
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
    let line = o["lines"][0]["id"].as_str().unwrap().to_owned();
    let mine = format!("/api/storefront/account/orders/{order}/returns");
    // Nothing shipped yet: nothing to return.
    send(
        &client,
        "acme",
        &customer,
        "POST",
        &mine,
        json!({ "lines": [{ "line_id": line, "quantity": 1 }] }),
        422,
    )
    .await;
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/fulfillments"),
        json!({}),
        201,
    )
    .await;

    let r = send(
        &client,
        "acme",
        &customer,
        "POST",
        &mine,
        json!({ "lines": [{ "line_id": line, "quantity": 1 }], "reason": "too small" }),
        201,
    )
    .await;
    let ret = r["id"].as_str().unwrap().to_owned();
    assert!(ret.starts_with("ret_"));
    assert_eq!(r["value_minor"], 1190);
    assert_eq!(r["requested_by"], s["customer"]["id"]);
    send(
        &client,
        "acme",
        &customer,
        "POST",
        &mine,
        json!({ "lines": [{ "line_id": line, "quantity": 2 }] }),
        422,
    )
    .await;
    let list = send(&client, "acme", &customer, "GET", &mine, Value::Null, 200).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{ret}/refund"),
        json!({}),
        409,
    )
    .await;
    let r = send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{ret}/receive"),
        json!({}),
        200,
    )
    .await;
    assert_eq!(r["status"], "received");
    assert_eq!(r["lines"][0]["location_id"], loc);
    let stock = send(
        &client,
        "acme",
        t,
        "GET",
        &format!("/api/inventory/{tee}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(stock["levels"][0]["on_hand"], 4);

    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{ret}/refund"),
        json!({ "amount_minor": 1191 }),
        422,
    )
    .await;
    let r = send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{ret}/refund"),
        json!({}),
        200,
    )
    .await;
    assert_eq!(r["status"], "refunded");
    assert_eq!(r["refunded_minor"], 1190);
    let p = send(
        &client,
        "acme",
        t,
        "GET",
        &format!("/api/payments/{pay}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["refunded_minor"], 1190);
    for _ in 0..3 {
        soat_events::dispatch_resources(&client.resources().await)
            .await
            .unwrap();
    }
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
    assert_eq!(o["financial_status"], "partially_refunded");

    // A goodwill refund, and no more than what is left.
    let done = send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/refunds"),
        json!({ "amount_minor": 100, "reason": "late" }),
        201,
    )
    .await;
    assert_eq!(done["refunds"][0]["refund"]["amount_minor"], 100);
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/refunds"),
        json!({ "amount_minor": 1586 }),
        422,
    )
    .await;

    // A rejected request frees its units for another.
    let r = send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/returns"),
        json!({ "lines": [{ "line_id": line, "quantity": 1 }] }),
        201,
    )
    .await;
    let second = r["id"].as_str().unwrap();
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{second}/reject"),
        Value::Null,
        200,
    )
    .await;
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/returns/{second}/receive"),
        json!({}),
        409,
    )
    .await;
    send(
        &client,
        "acme",
        t,
        "POST",
        &format!("/api/orders/{order}/returns"),
        json!({ "lines": [{ "line_id": line, "quantity": 1 }] }),
        201,
    )
    .await;

    // Another shop reaches none of it.
    send(
        &client,
        "globex",
        t,
        "GET",
        &format!("/api/orders/{order}/returns"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "globex",
        t,
        "POST",
        &format!("/api/returns/{ret}/receive"),
        json!({}),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        t,
        "POST",
        &format!("/api/returns/{second}/reject"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "globex",
        t,
        "POST",
        &format!("/api/orders/{order}/refunds"),
        json!({ "amount_minor": 1 }),
        404,
    )
    .await;
}
