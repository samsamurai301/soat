//! C17: invoices and credit notes.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

#[allow(clippy::too_many_arguments)]
async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    headers: &[(&str, &str)],
    body: Value,
    status: u16,
) -> (Value, String) {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = headers.iter().fold(req, |r, (k, v)| r.header(*k, *v));
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    let text = res.text();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (json, text)
}

struct Shop {
    client: TestClient,
    admin: String,
    tee: String,
    book: String,
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
            Some(&self.admin),
            &[],
            body,
            status,
        )
        .await
        .0
    }

    async fn page(&self, path: &str, headers: &[(&str, &str)], status: u16) -> String {
        let bearer = if headers.is_empty() {
            Some(self.admin.as_str())
        } else {
            None
        };
        call(
            &self.client,
            "acme",
            "GET",
            path,
            bearer,
            headers,
            Value::Null,
            status,
        )
        .await
        .1
    }

    /// Places an order for two tees and a book to `address`; returns it
    /// with its token.
    async fn order(&self, address: Value, vat: Option<&str>) -> (Value, String) {
        let (c, _) = call(
            &self.client,
            "acme",
            "POST",
            "/api/storefront/carts",
            None,
            &[],
            json!({ "currency": "EUR" }),
            201,
        )
        .await;
        let id = c["cart"]["id"].as_str().unwrap().to_owned();
        let token = c["token"].as_str().unwrap().to_owned();
        let shopper = |method: &'static str, path: String, body: Value, status: u16| {
            let token = token.clone();
            async move {
                call(
                    &self.client,
                    "acme",
                    method,
                    &path,
                    None,
                    &[("x-soat-cart-token", &token)],
                    body,
                    status,
                )
                .await
                .0
            }
        };
        for (item, q) in [(&self.tee, 2), (&self.book, 1)] {
            shopper(
                "POST",
                format!("/api/storefront/carts/{id}/lines"),
                json!({ "item_id": item, "quantity": q }),
                200,
            )
            .await;
        }
        let path = format!("/api/storefront/carts/{id}/checkout");
        shopper(
            "PATCH",
            path.clone(),
            json!({ "email": "ada@example.com", "shipping_address": address, "vat_number": vat, "shipping_rate_id": self.rate }),
            200,
        )
        .await;
        let done = shopper(
            "POST",
            format!("{path}/complete"),
            json!({ "payment_method_id": self.invoice }),
            201,
        )
        .await;
        (
            done["order"].clone(),
            done["order_token"].as_str().unwrap().to_owned(),
        )
    }

    async fn dispatch(&self) {
        soat_events::dispatch_resources(&self.client.resources().await)
            .await
            .unwrap();
    }

    async fn pay(&self, order: &str) {
        let p = self
            .staff(
                "GET",
                &format!("/api/payments?reference={order}"),
                Value::Null,
                200,
            )
            .await;
        let id = p["items"][0]["id"].as_str().unwrap();
        self.staff(
            "POST",
            &format!("/api/payments/{id}/mark-paid"),
            json!({}),
            200,
        )
        .await;
    }

    async fn invoices(&self, order: &str) -> Vec<Value> {
        let page = self
            .staff(
                "GET",
                &format!("/api/invoices?order_id={order}&sort=id"),
                Value::Null,
                200,
            )
            .await;
        page["items"].as_array().unwrap().clone()
    }

    async fn settings(&self, issue_on: &str) {
        self.staff(
            "PUT",
            "/api/invoices/settings",
            json!({
                "legal_name": "Acme GmbH",
                "address": { "line1": "Torstr. 5", "postal_code": "10119", "city": "Berlin", "country": "DE" },
                "vat_number": "DE 123456789",
                "tax_number": "30/123/45678",
                "bank_details": "IBAN DE02 1203 0000 0000 2020 51",
                "footer": "Amtsgericht Charlottenburg HRB 12345",
                "issue_on": issue_on,
            }),
            200,
        )
        .await;
    }
}

fn berlin() -> Value {
    json!({ "first_name": "Ada", "last_name": "Lovelace", "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" })
}

/// A German shop, prices including VAT: an 11.90 tee (19%), a 10.70 book
/// (7%), 4.95 shipping to Germany and Austria, invoice payment.
async fn shop() -> Shop {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let mut s = Shop {
        client,
        admin,
        tee: String::new(),
        book: String::new(),
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
    for (sku, price, class) in [("TEE", 1190, "standard"), ("BOOK", 1070, "reduced")] {
        let p = s
            .staff(
                "POST",
                "/api/products",
                json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR", "tax_class": class }),
                201,
            )
            .await;
        let id = p["id"].as_str().unwrap().to_owned();
        s.staff(
            "POST",
            &format!("/api/inventory/{id}/adjustments"),
            json!({ "location_id": loc, "delta": 20, "reason": "received" }),
            200,
        )
        .await;
        if sku == "TEE" {
            s.tee = id;
        } else {
            s.book = id;
        }
    }
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
            json!({ "name": "DACH", "countries": ["DE", "AT"] }),
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
async fn paid_orders_are_invoiced_and_refunds_credited() {
    let s = shop().await;
    s.staff("GET", "/api/invoices/settings", Value::Null, 404)
        .await;
    s.settings("paid").await;
    let (order, token) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.dispatch().await;
    assert!(s.invoices(id).await.is_empty(), "not before payment");

    s.pay(id).await;
    s.dispatch().await;
    s.dispatch().await;
    let all = s.invoices(id).await;
    assert_eq!(all.len(), 1);
    let inv = &all[0];
    let number = inv["number"].as_str().unwrap();
    assert!(
        number.starts_with("INV-20") && number.ends_with("-00001"),
        "{number}"
    );
    assert_eq!(inv["kind"], "invoice");
    assert_eq!(inv["order_number"], 1001);
    assert_eq!(inv["seller"]["name"], "Acme GmbH");
    assert_eq!(inv["seller"]["vat_number"], "DE123456789");
    assert_eq!(inv["buyer"]["name"], "Ada Lovelace");
    assert_eq!(inv["lines"].as_array().unwrap().len(), 3);
    assert_eq!(inv["lines"][2]["description"], "Shipping: Standard");
    assert_eq!(inv["lines"][2]["rate_bps"], 1900);
    assert_eq!(
        inv["rates"],
        json!([
            { "rate_bps": 1900, "net_minor": 2416, "tax_minor": 459, "gross_minor": 2875 },
            { "rate_bps": 700, "net_minor": 1000, "tax_minor": 70, "gross_minor": 1070 },
        ])
    );
    assert_eq!(inv["total_minor"], 3945);
    assert_eq!(inv["tax_minor"], 529);
    assert_eq!(inv["net_minor"], 3416);
    let inv_id = inv["id"].as_str().unwrap();

    // A refund credits the invoice by itself, once.
    s.staff(
        "POST",
        &format!("/api/orders/{id}/refunds"),
        json!({ "amount_minor": 1000, "reason": "Damaged" }),
        201,
    )
    .await;
    s.dispatch().await;
    s.dispatch().await;
    let all = s.invoices(id).await;
    assert_eq!(all.len(), 2);
    let note = &all[1];
    assert_eq!(note["kind"], "credit_note");
    assert!(note["number"].as_str().unwrap().ends_with("-00001"));
    assert!(note["number"].as_str().unwrap().starts_with("CN-"));
    assert_eq!(note["credits_id"], inv_id);
    assert_eq!(note["total_minor"], 1000);
    assert_eq!(note["rates"][0]["gross_minor"], 729);
    assert_eq!(note["rates"][0]["tax_minor"], 116);
    assert_eq!(note["rates"][1]["tax_minor"], 18);
    assert_eq!(
        note["lines"][0]["description"],
        format!("Credit for invoice {number}: Damaged")
    );
    assert_eq!(all[0]["credited_minor"], 1000);

    // Staff credit the rest; nothing is left after.
    let path = format!("/api/invoices/{inv_id}/credit-notes");
    s.staff("POST", &path, json!({ "amount_minor": 2946 }), 422)
        .await;
    let rest = s
        .staff("POST", &path, json!({ "reason": "Order canceled" }), 201)
        .await;
    assert_eq!(rest["total_minor"], 2945);
    assert!(rest["number"].as_str().unwrap().ends_with("-00002"));
    s.staff("POST", &path, json!({}), 422).await;
    let credit = all[1]["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/invoices/{credit}/credit-notes"),
        json!({}),
        404,
    )
    .await;

    // Printable, for staff and for the buyer holding the order's token.
    let html = s
        .page(&format!("/api/invoices/{inv_id}/document"), &[], 200)
        .await;
    assert!(html.contains(&format!("Invoice {number}")), "{html}");
    assert!(html.contains("VAT ID DE123456789"));
    assert!(html.contains("<td>19%</td>"));
    assert!(html.contains("39.45 EUR"));
    assert!(html.contains("HRB 12345"));
    let mine = s
        .page(
            &format!("/api/storefront/orders/{id}/invoices"),
            &[("x-soat-order-token", &token)],
            200,
        )
        .await;
    let mine: Value = serde_json::from_str(&mine).unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 3);
    let doc = s
        .page(
            &format!("/api/storefront/orders/{id}/invoices/{credit}/document"),
            &[("x-soat-order-token", &token)],
            200,
        )
        .await;
    assert!(doc.contains(&format!("Credits invoice {number}")), "{doc}");
    s.page(
        &format!("/api/storefront/orders/{id}/invoices"),
        &[("x-soat-order-token", "soat_order_wrong")],
        404,
    )
    .await;
}

#[tokio::test]
async fn manual_issue_and_reverse_charge() {
    let s = shop().await;
    let (order, _) = s.order(berlin(), None).await;
    let id = order["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        422,
    )
    .await;
    s.settings("manual").await;
    s.pay(id).await;
    s.dispatch().await;
    assert!(s.invoices(id).await.is_empty());
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        201,
    )
    .await;
    s.staff(
        "POST",
        &format!("/api/orders/{id}/invoice"),
        Value::Null,
        409,
    )
    .await;

    // An Austrian business with a VAT number: no German VAT, and the note.
    let vienna = json!({ "company": "Wien AG", "line1": "Ring 1", "postal_code": "1010", "city": "Wien", "country": "AT" });
    let (order, _) = s.order(vienna, Some("ATU12345678")).await;
    let id = order["id"].as_str().unwrap();
    let inv = s
        .staff(
            "POST",
            &format!("/api/orders/{id}/invoice"),
            Value::Null,
            201,
        )
        .await;
    assert!(inv["number"].as_str().unwrap().ends_with("-00002"));
    assert_eq!(inv["reverse_charge"], true);
    assert_eq!(inv["tax_minor"], 0);
    assert_eq!(inv["buyer"]["name"], "Wien AG");
    assert_eq!(inv["buyer"]["vat_number"], "ATU12345678");
    assert!(inv["note"].as_str().unwrap().starts_with("Reverse charge"));
    assert_eq!(inv["total_minor"], order["total_minor"]);

    s.staff(
        "PUT",
        "/api/invoices/settings",
        json!({ "legal_name": "X", "address": berlin(), "issue_on": "weekly" }),
        422,
    )
    .await;
    s.staff(
        "PUT",
        "/api/invoices/settings",
        json!({ "legal_name": "X", "address": berlin(), "invoice_prefix": "<b>" }),
        422,
    )
    .await;
}

#[tokio::test]
async fn invoices_stay_in_their_shop() {
    let s = shop().await;
    s.settings("placed").await;
    let (order, _) = s.order(berlin(), None).await;
    s.dispatch().await;
    let all = s.invoices(order["id"].as_str().unwrap()).await;
    assert_eq!(all.len(), 1, "issued when placed");
    let id = all[0]["id"].as_str().unwrap();
    let other = |method: &'static str, path: String, status: u16| {
        let s = &s;
        async move {
            call(
                &s.client,
                "other",
                method,
                &path,
                Some(&s.admin),
                &[],
                if method == "POST" {
                    json!({})
                } else {
                    Value::Null
                },
                status,
            )
            .await
            .0
        }
    };
    other("GET", format!("/api/invoices/{id}"), 404).await;
    other("GET", format!("/api/invoices/{id}/document"), 404).await;
    other("POST", format!("/api/invoices/{id}/credit-notes"), 404).await;
    other(
        "POST",
        format!("/api/orders/{}/invoice", order["id"].as_str().unwrap()),
        404,
    )
    .await;
    let list = other("GET", "/api/invoices".into(), 200).await;
    assert!(list["items"].as_array().unwrap().is_empty());
    other("GET", "/api/invoices/settings".into(), 404).await;
    call(
        &s.client,
        "acme",
        "GET",
        "/api/invoices",
        None,
        &[],
        Value::Null,
        401,
    )
    .await;
}
