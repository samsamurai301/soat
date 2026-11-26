//! C20: draft orders put together by staff.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    auth: (Option<&str>, Option<&str>),
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
    let req = match auth.0 {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = match auth.1 {
        Some(t) => req.header("x-soat-cart-token", t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 300 || status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

struct Shop {
    client: TestClient,
    admin: String,
    tee: String,
    rate: String,
    invoice: String,
}

impl Shop {
    async fn staff(&self, method: &str, path: &str, body: Value, status: u16) -> Value {
        call(
            &self.client,
            "acme",
            method,
            path,
            (Some(&self.admin), None),
            body,
            status,
        )
        .await
    }
}

/// A German shop, prices including VAT: a 11.90 tee, 4.95 shipping, and
/// invoice payment.
async fn shop() -> Shop {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let mut s = Shop {
        client,
        admin,
        tee: String::new(),
        rate: String::new(),
        invoice: String::new(),
    };
    let loc = s
        .staff(
            "POST",
            "/api/locations",
            json!({ "name": "W", "country": "DE" }),
            201,
        )
        .await["id"]
        .clone();
    let p = s
        .staff(
            "POST",
            "/api/products",
            json!({ "sku": "TEE", "title": "Tee", "price_minor": 1190, "currency": "EUR" }),
            201,
        )
        .await;
    s.tee = p["id"].as_str().unwrap().to_owned();
    s.staff(
        "POST",
        &format!("/api/inventory/{}/adjustments", s.tee),
        json!({ "location_id": loc, "delta": 20, "reason": "received" }),
        200,
    )
    .await;
    s.staff(
        "PUT",
        "/api/tax/settings",
        json!({ "origin_country": "DE", "prices_include_tax": true, "charge_destination_vat": false }),
        200,
    )
    .await;
    s.staff("POST", "/api/tax/presets/eu-vat", Value::Null, 201)
        .await;
    let z = s
        .staff(
            "POST",
            "/api/shipping/zones",
            json!({ "name": "DE", "countries": ["DE"] }),
            201,
        )
        .await;
    let r = s
        .staff(
            "POST",
            &format!("/api/shipping/zones/{}/rates", z["id"].as_str().unwrap()),
            json!({ "name": "Standard", "currency": "EUR", "price_minor": 495 }),
            201,
        )
        .await;
    s.rate = r["id"].as_str().unwrap().to_owned();
    let m = s
        .staff(
            "POST",
            "/api/payment-methods",
            json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
            201,
        )
        .await;
    s.invoice = m["id"].as_str().unwrap().to_owned();
    s
}

const BERLIN: &str =
    r#"{ "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" }"#;

#[tokio::test]
async fn staff_place_orders_from_drafts() {
    let s = shop().await;
    let addr: Value = serde_json::from_str(BERLIN).unwrap();
    let cus = s
        .staff(
            "POST",
            "/api/customers",
            json!({ "email": "ada@example.com" }),
            201,
        )
        .await;
    for bad in [
        json!({ "currency": "EUR", "lines": [{ "item_id": "prod_nope", "quantity": 1 }] }),
        json!({ "currency": "EUR", "customer_id": "cus_nope" }),
        json!({ "currency": "EUR", "lines": [{ "item_id": s.tee, "quantity": 0 }] }),
    ] {
        s.staff("POST", "/api/draft-orders", bad, 422).await;
    }
    let d = s
        .staff(
            "POST",
            "/api/draft-orders",
            json!({ "currency": "EUR", "customer_id": cus["id"],
                    "lines": [{ "item_id": s.tee, "quantity": 2 }, { "item_id": s.tee, "quantity": 1, "unit_price_minor": 990 }] }),
            201,
        )
        .await;
    assert_eq!(d["status"], "open");
    assert_eq!(d["email"], "ada@example.com");
    assert_eq!(d["subtotal_minor"], 2 * 1190 + 990);
    assert_eq!(d["lines"][1]["custom_price"], true);
    assert_eq!(d["tax_minor"], 0);
    let id = d["id"].as_str().unwrap().to_owned();
    let path = format!("/api/draft-orders/{id}");

    // No address yet: it cannot be placed.
    s.staff("POST", &format!("{path}/complete"), json!({}), 422)
        .await;
    let d = s
        .staff(
            "PUT",
            &path,
            json!({ "currency": "EUR", "customer_id": cus["id"], "shipping_address": addr,
                    "lines": [{ "item_id": s.tee, "quantity": 2 }],
                    "shipping_name": "Courier", "shipping_price_minor": 800,
                    "discount_minor": 380, "note": "Phone order" }),
            200,
        )
        .await;
    assert_eq!(d["subtotal_minor"], 2380);
    assert_eq!(d["discount_minor"], 380);
    // Prices include VAT: 2000 of goods holds 319, 800 of shipping 128.
    assert_eq!(d["tax_minor"], 319 + 128);
    assert_eq!(d["total_minor"], 2380 - 380 + 800);

    let done = s
        .staff(
            "POST",
            &format!("{path}/complete"),
            json!({ "paid": true }),
            201,
        )
        .await;
    let order = &done["order"];
    assert_eq!(order["total_minor"], 2800);
    assert_eq!(order["financial_status"], "paid");
    assert_eq!(order["customer_id"], cus["id"]);
    assert_eq!(order["note"], "Phone order");
    assert_eq!(order["shipping_name"], "Courier");
    assert_eq!(done["draft_order"]["status"], "completed");
    assert_eq!(done["draft_order"]["order_id"], order["id"]);
    assert!(done["payment"].is_null());
    s.staff("PUT", &path, json!({ "currency": "EUR" }), 409)
        .await;
    s.staff("POST", &format!("{path}/complete"), json!({}), 409)
        .await;

    // Another draft, paid by invoice: a payment starts.
    let d = s
        .staff(
            "POST",
            "/api/draft-orders",
            json!({ "currency": "EUR", "email": "bob@example.com", "shipping_address": addr,
                    "lines": [{ "item_id": s.tee, "quantity": 1 }] }),
            201,
        )
        .await;
    let done = s
        .staff(
            "POST",
            &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
            json!({ "payment_method_id": s.invoice }),
            201,
        )
        .await;
    assert_eq!(done["payment"]["amount_minor"], 1190);
    assert_eq!(done["order"]["financial_status"], "pending");
    let open = s
        .staff("GET", "/api/draft-orders?status=open", Value::Null, 200)
        .await;
    assert_eq!(open["total"], 0);
    let events = s
        .staff(
            "GET",
            "/api/events?kind=draft_order.completed",
            Value::Null,
            200,
        )
        .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 2);

    // Another shop sees none of it.
    let other = |method: &'static str, path: String, body: Value, status: u16| {
        let s = &s;
        async move {
            call(
                &s.client,
                "other",
                method,
                &path,
                (Some(&s.admin), None),
                body,
                status,
            )
            .await
        }
    };
    other("GET", path.clone(), Value::Null, 404).await;
    other("PUT", path.clone(), json!({ "currency": "EUR" }), 404).await;
    other("POST", format!("{path}/complete"), json!({}), 404).await;
    other("DELETE", path.clone(), Value::Null, 404).await;
    let list = other("GET", "/api/draft-orders".into(), Value::Null, 200).await;
    assert_eq!(list["total"], 0);
    s.staff("DELETE", &path, Value::Null, 204).await;
    s.staff("GET", &path, Value::Null, 404).await;
}

#[tokio::test]
async fn staff_edit_open_orders() {
    let s = shop().await;
    let addr: Value = serde_json::from_str(BERLIN).unwrap();
    let d = s
        .staff(
            "POST",
            "/api/draft-orders",
            json!({ "currency": "EUR", "email": "ada@example.com", "shipping_address": addr,
                    "lines": [{ "item_id": s.tee, "quantity": 2 }],
                    "shipping_name": "Courier", "shipping_price_minor": 800, "discount_minor": 380 }),
            201,
        )
        .await;
    let done = s
        .staff(
            "POST",
            &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
            json!({ "paid": true }),
            201,
        )
        .await;
    let order = done["order"]["id"].as_str().unwrap().to_owned();
    let line = done["order"]["lines"][0]["id"].as_str().unwrap().to_owned();
    let edit = format!("/api/orders/{order}/edit");

    let e = s
        .staff("POST", &edit, json!({ "lines": [{ "line_id": line, "quantity": 3 }], "reason": "Asked for one more" }), 200)
        .await;
    assert_eq!(e["previous_total_minor"], 2800);
    assert_eq!(e["order"]["lines"][0]["quantity"], 3);
    assert_eq!(e["order"]["lines"][0]["discount_minor"], 570);
    assert_eq!(e["order"]["total_minor"], 3570 - 570 + 800);
    assert_eq!(e["balance_minor"], 1000);
    assert_eq!(e["order"]["financial_status"], "partially_paid");

    let e = s
        .staff(
            "POST",
            &edit,
            json!({ "add": [{ "item_id": s.tee, "quantity": 1, "unit_price_minor": 500 }] }),
            200,
        )
        .await;
    let added = e["order"]["lines"][1].clone();
    assert_eq!(added["unit_price_minor"], 500);
    assert_eq!(added["tax_rate_bps"], 1900);
    assert_eq!(e["order"]["total_minor"], 3800 + 500);

    // Back to two tees and nothing added: the buyer is owed nothing more.
    let e = s
        .staff(
            "POST",
            &edit,
            json!({ "lines": [{ "line_id": line, "quantity": 2 }, { "line_id": added["id"], "quantity": 0 }] }),
            200,
        )
        .await;
    assert_eq!(e["order"]["lines"].as_array().unwrap().len(), 1);
    assert_eq!(e["order"]["total_minor"], 2800);
    assert_eq!(e["balance_minor"], 0);
    assert_eq!(e["order"]["financial_status"], "paid");

    for (bad, status) in [
        (json!({}), 422),
        (
            json!({ "lines": [{ "line_id": "ol_nope", "quantity": 1 }] }),
            422,
        ),
        (
            json!({ "lines": [{ "line_id": line, "quantity": 0 }] }),
            422,
        ),
        (
            json!({ "add": [{ "item_id": s.tee, "quantity": 100 }] }),
            409,
        ),
    ] {
        s.staff("POST", &edit, bad, status).await;
    }
    let events = s
        .staff("GET", "/api/events?kind=order.edited", Value::Null, 200)
        .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 3);
    call(
        &s.client,
        "other",
        "POST",
        &edit,
        (Some(&s.admin), None),
        json!({ "lines": [{ "line_id": line, "quantity": 1 }] }),
        404,
    )
    .await;
}
