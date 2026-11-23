//! C16: transactional email on order events, templates shops reword, and
//! the sent log.

mod common;

use serde_json::{json, Value};
use zapline::mail::{Email, Mailer};
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

    async fn shopper(
        &self,
        method: &str,
        path: &str,
        cart: &str,
        body: Value,
        status: u16,
    ) -> Value {
        call(
            &self.client,
            "acme",
            method,
            path,
            (None, Some(cart)),
            body,
            status,
        )
        .await
    }

    /// Places an order for two tees, paid by invoice; returns it.
    async fn order(&self, email: &str) -> Value {
        let c = call(
            &self.client,
            "acme",
            "POST",
            "/api/storefront/carts",
            (None, None),
            json!({ "currency": "EUR" }),
            201,
        )
        .await;
        let id = c["cart"]["id"].as_str().unwrap().to_owned();
        let token = c["token"].as_str().unwrap().to_owned();
        self.shopper(
            "POST",
            &format!("/api/storefront/carts/{id}/lines"),
            &token,
            json!({ "item_id": self.tee, "quantity": 2 }),
            200,
        )
        .await;
        let path = format!("/api/storefront/carts/{id}/checkout");
        self.shopper(
            "PATCH",
            &path,
            &token,
            json!({
                "email": email,
                "shipping_address": { "first_name": "Ada", "last_name": "Lovelace", "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" },
                "shipping_rate_id": self.rate,
            }),
            200,
        )
        .await;
        let done = self
            .shopper(
                "POST",
                &format!("{path}/complete"),
                &token,
                json!({ "payment_method_id": self.invoice }),
                201,
            )
            .await;
        done["order"].clone()
    }

    async fn dispatch(&self) {
        soat_events::dispatch_resources(&self.client.resources().await)
            .await
            .unwrap();
    }

    async fn sent(&self) -> Vec<Email> {
        let mailer: Mailer = self.client.resources().await.require().unwrap();
        mailer.sent()
    }
}

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
            json!({ "sku": "TEE", "title": "Linen <Tee>", "price_minor": 1190, "currency": "EUR" }),
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

#[tokio::test]
async fn order_events_send_email() {
    let s = shop().await;
    let settings = s
        .staff("GET", "/api/notifications/settings", Value::Null, 200)
        .await;
    assert_eq!(settings["disabled"], json!(["payment_received"]));
    s.staff(
        "PUT",
        "/api/notifications/settings",
        json!({ "sender_name": "Acme Store", "reply_to": "help@acme.test", "staff_email": "orders@acme.test", "disabled": [] }),
        200,
    )
    .await;

    let order = s.order("Ada@Example.com").await;
    let id = order["id"].as_str().unwrap();
    s.dispatch().await;
    s.dispatch().await;
    let sent = s.sent().await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    let confirmation = sent.iter().find(|e| e.to == ["ada@example.com"]).unwrap();
    assert_eq!(confirmation.subject, "Order 1001 at acme");
    assert_eq!(
        confirmation.from.as_deref(),
        Some("\"Acme Store\" <shops@soat.test>")
    );
    assert_eq!(confirmation.reply_to.as_deref(), Some("help@acme.test"));
    let text = confirmation.text.as_deref().unwrap();
    assert!(text.contains("2 x Linen <Tee>  23.80 EUR"), "{text}");
    assert!(text.contains("Total: 28.75 EUR"), "{text}");
    assert!(
        text.contains("Ada Lovelace\nHauptstr. 1\n10115 Berlin\nDE"),
        "{text}"
    );
    let html = confirmation.html.as_deref().unwrap();
    assert!(html.contains("Linen &lt;Tee&gt;"), "{html}");
    let staff = sent.iter().find(|e| e.to == ["orders@acme.test"]).unwrap();
    assert_eq!(staff.subject, "New order 1001: 28.75 EUR");

    let log = s
        .staff(
            "GET",
            &format!("/api/notifications/messages?order_id={id}"),
            Value::Null,
            200,
        )
        .await;
    assert_eq!(log["items"].as_array().unwrap().len(), 2);
    assert_eq!(log["items"][0]["status"], "sent");
    assert_eq!(log["items"][0]["attempts"], 1);

    // Paid, shipped, refunded.
    let payments = s
        .staff(
            "GET",
            &format!("/api/payments?reference={id}"),
            Value::Null,
            200,
        )
        .await;
    let payment = payments["items"][0]["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/payments/{payment}/mark-paid"),
        json!({}),
        200,
    )
    .await;
    s.dispatch().await;
    s.staff(
        "POST",
        &format!("/api/orders/{id}/fulfillments"),
        json!({ "carrier": "DHL", "tracking_number": "0034", "tracking_url": "https://track.example/0034" }),
        201,
    )
    .await;
    s.staff(
        "POST",
        &format!("/api/orders/{id}/refunds"),
        json!({ "amount_minor": 500, "reason": "Late delivery" }),
        201,
    )
    .await;
    s.dispatch().await;
    let sent = s.sent().await;
    let subjects: Vec<&str> = sent.iter().map(|e| e.subject.as_str()).collect();
    assert_eq!(
        subjects,
        [
            "Order 1001 at acme",
            "New order 1001: 28.75 EUR",
            "Payment received for order 1001",
            "Order 1001 is on its way",
            "Refund for order 1001",
        ],
        "{subjects:?}"
    );
    let shipped = sent[3].text.as_deref().unwrap();
    assert!(shipped.contains("2 x Linen <Tee>"), "{shipped}");
    assert!(shipped.contains("Tracking number: 0034"), "{shipped}");
    assert!(sent[4]
        .text
        .as_deref()
        .unwrap()
        .contains("We have refunded 5.00 EUR for order 1001.\nReason: Late delivery"));

    // Resending one.
    let first = log["items"][1]["id"].as_str().unwrap();
    let again = s
        .staff(
            "POST",
            &format!("/api/notifications/messages/{first}/resend"),
            Value::Null,
            200,
        )
        .await;
    assert_eq!(again["attempts"], 2);
    assert_eq!(s.sent().await.len(), 6);
}

#[tokio::test]
async fn cancellation_and_disabled_templates() {
    let s = shop().await;
    s.staff(
        "PUT",
        "/api/notifications/settings",
        json!({ "disabled": ["order_confirmation"] }),
        200,
    )
    .await;
    s.staff(
        "PUT",
        "/api/notifications/settings",
        json!({ "disabled": ["newsletter"] }),
        422,
    )
    .await;
    let order = s.order("ada@example.com").await;
    s.staff(
        "POST",
        &format!("/api/orders/{}/cancel", order["id"].as_str().unwrap()),
        json!({ "reason": "Out of stock" }),
        200,
    )
    .await;
    s.dispatch().await;
    let sent = s.sent().await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].subject, "Order 1001 has been canceled");
    assert!(sent[0]
        .text
        .as_deref()
        .unwrap()
        .contains("Reason: Out of stock"));
}

#[tokio::test]
async fn shops_reword_templates() {
    let s = shop().await;
    let all = s
        .staff("GET", "/api/notifications/templates", Value::Null, 200)
        .await;
    assert_eq!(all.as_array().unwrap().len(), 6);
    assert_eq!(all[0]["key"], "order_confirmation");
    assert_eq!(all[0]["customized"], false);
    assert_eq!(all[1]["enabled"], false);
    s.staff("GET", "/api/notifications/templates/nope", Value::Null, 404)
        .await;

    for bad in [
        json!({ "subject": "Hi {{ order.number", "text": "x" }),
        json!({ "subject": "Hi", "text": "{{#each order.lines}}" }),
        json!({ "subject": "Hi", "text": "x", "html": "{{> evil}}" }),
    ] {
        s.staff(
            "PUT",
            "/api/notifications/templates/order_confirmation",
            bad,
            422,
        )
        .await;
    }
    let t = s
        .staff(
            "PUT",
            "/api/notifications/templates/order_confirmation",
            json!({
                "subject": "Danke! Bestellung {{order.number}}",
                "text": "{{#each order.lines}}{{quantity}}× {{title}}\n{{/each}}Summe: {{order.total}}",
            }),
            200,
        )
        .await;
    assert_eq!(t["customized"], true);

    let preview = s
        .staff(
            "POST",
            "/api/notifications/templates/order_confirmation/preview",
            json!({}),
            200,
        )
        .await;
    assert_eq!(preview["subject"], "Danke! Bestellung 1001");
    assert!(preview["text"]
        .as_str()
        .unwrap()
        .starts_with("2× Linen shirt\n"));
    let preview = s
        .staff(
            "POST",
            "/api/notifications/templates/shipping_confirmation/preview",
            json!({ "html": "<a href=\"{{fulfillment.tracking_url}}\">{{shop.name}}</a>" }),
            200,
        )
        .await;
    assert!(preview["html"].as_str().unwrap().ends_with(">acme</a>"));

    s.order("ada@example.com").await;
    s.dispatch().await;
    let sent = s.sent().await;
    assert_eq!(sent[0].subject, "Danke! Bestellung 1001");
    assert_eq!(
        sent[0].text.as_deref(),
        Some("2× Linen <Tee>\nSumme: 28.75 EUR")
    );
    // No HTML part when the shop's wording has none.
    assert!(sent[0].html.is_none());

    let t = s
        .staff(
            "DELETE",
            "/api/notifications/templates/order_confirmation",
            Value::Null,
            200,
        )
        .await;
    assert_eq!(t["customized"], false);
    assert_eq!(t["subject"], "Order {{order.number}} at {{shop.name}}");
}

#[tokio::test]
async fn email_stays_in_its_shop() {
    let s = shop().await;
    s.order("ada@example.com").await;
    s.dispatch().await;
    let log = s
        .staff("GET", "/api/notifications/messages", Value::Null, 200)
        .await;
    let id = log["items"][0]["id"].as_str().unwrap();
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
    other(
        "GET",
        format!("/api/notifications/messages/{id}"),
        Value::Null,
        404,
    )
    .await;
    other(
        "POST",
        format!("/api/notifications/messages/{id}/resend"),
        Value::Null,
        404,
    )
    .await;
    let list = other(
        "GET",
        "/api/notifications/messages".into(),
        Value::Null,
        200,
    )
    .await;
    assert!(list["items"].as_array().unwrap().is_empty());
    other(
        "PUT",
        "/api/notifications/templates/order_confirmation".into(),
        json!({ "subject": "Other", "text": "x" }),
        200,
    )
    .await;
    let mine = s
        .staff(
            "GET",
            "/api/notifications/templates/order_confirmation",
            Value::Null,
            200,
        )
        .await;
    assert_eq!(mine["customized"], false);
    call(
        &s.client,
        "acme",
        "GET",
        "/api/notifications/messages",
        (None, None),
        Value::Null,
        401,
    )
    .await;
}
