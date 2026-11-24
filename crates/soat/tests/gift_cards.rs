//! C15: gift cards and store credit, issued by staff and spent at
//! checkout.

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

    async fn card(&self, body: Value) -> Value {
        self.staff("POST", "/api/gift-cards", body, 201).await
    }

    async fn dispatch(&self) {
        for _ in 0..3 {
            soat_events::dispatch_resources(&self.client.resources().await)
                .await
                .unwrap();
        }
    }

    /// A cart with two tees, an email, a Berlin address and shipping
    /// chosen; returns its checkout path and token.
    async fn checkout(&self, email: &str) -> (String, String) {
        let tee = self.tee.clone();
        self.checkout_with(email, &[(&tee, 2)]).await
    }

    /// A cart with `lines`, an email, a Berlin address and shipping chosen.
    async fn checkout_with(&self, email: &str, lines: &[(&str, i64)]) -> (String, String) {
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
        for (item, quantity) in lines {
            self.shopper(
                "POST",
                &format!("/api/storefront/carts/{id}/lines"),
                &token,
                json!({ "item_id": item, "quantity": quantity }),
                200,
            )
            .await;
        }
        let path = format!("/api/storefront/carts/{id}/checkout");
        self.shopper(
            "PATCH",
            &path,
            &token,
            json!({
                "email": email,
                "shipping_address": { "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" },
                "shipping_rate_id": self.rate,
            }),
            200,
        )
        .await;
        (path, token)
    }

    async fn complete(&self, path: &str, token: &str, status: u16) -> Value {
        self.shopper(
            "POST",
            &format!("{path}/complete"),
            token,
            json!({ "payment_method_id": self.invoice }),
            status,
        )
        .await
    }

    async fn balance(&self, id: &str) -> i64 {
        self.staff("GET", &format!("/api/gift-cards/{id}"), Value::Null, 200)
            .await["balance_minor"]
            .as_i64()
            .unwrap()
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

#[tokio::test]
async fn staff_issue_gift_cards_and_store_credit() {
    let s = shop().await;
    let issued = s
        .card(json!({ "amount_minor": 2000, "currency": "EUR", "note": "Raffle prize" }))
        .await;
    let code = issued["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 19);
    assert_eq!(issued["kind"], "gift_card");
    assert_eq!(issued["balance_minor"], 2000);
    assert_eq!(issued["last_chars"], code[15..]);
    let id = issued["id"].as_str().unwrap().to_owned();
    let got = s
        .staff("GET", &format!("/api/gift-cards/{id}"), Value::Null, 200)
        .await;
    assert!(got.get("code").is_none());
    assert_eq!(got["transactions"][0]["kind"], "issued");
    let events = s
        .staff(
            "GET",
            "/api/events?kind=gift_card.created",
            Value::Null,
            200,
        )
        .await;
    assert!(!events.to_string().contains(&code));

    let got = s
        .staff(
            "POST",
            &format!("/api/gift-cards/{id}/adjustments"),
            json!({ "amount_minor": -500, "note": "Typo" }),
            200,
        )
        .await;
    assert_eq!(got["balance_minor"], 1500);
    assert_eq!(got["transactions"][0]["amount_minor"], -500);
    s.staff(
        "POST",
        &format!("/api/gift-cards/{id}/adjustments"),
        json!({ "amount_minor": -1501 }),
        422,
    )
    .await;

    // Own codes, typed loosely; taken ones are a 409.
    let own = s
        .card(json!({ "amount_minor": 100, "currency": "EUR", "code": "xmas-2026-abcd" }))
        .await;
    assert_eq!(own["code"], "XMAS2026ABCD");
    s.staff(
        "POST",
        "/api/gift-cards",
        json!({ "amount_minor": 100, "currency": "EUR", "code": "XMAS 2026 ABCD" }),
        409,
    )
    .await;
    for bad in [
        json!({ "amount_minor": 100, "currency": "EUR", "code": "short" }),
        json!({ "kind": "store_credit", "amount_minor": 100, "currency": "EUR" }),
        json!({ "kind": "store_credit", "amount_minor": 100, "currency": "EUR", "customer_id": "cus_nope" }),
        json!({ "kind": "voucher", "amount_minor": 100, "currency": "EUR" }),
        json!({ "amount_minor": 100, "currency": "euro" }),
    ] {
        s.staff("POST", "/api/gift-cards", bad, 422).await;
    }
    let cus = s
        .staff(
            "POST",
            "/api/customers",
            json!({ "email": "ada@example.com" }),
            201,
        )
        .await;
    let credit = s
        .card(json!({ "kind": "store_credit", "amount_minor": 700, "currency": "EUR", "customer_id": cus["id"] }))
        .await;
    assert!(credit["code"].is_null());
    s.staff(
        "POST",
        "/api/gift-cards",
        json!({ "kind": "store_credit", "amount_minor": 1, "currency": "EUR", "customer_id": cus["id"] }),
        409,
    )
    .await;
    let list = s
        .staff("GET", "/api/gift-cards?kind=store_credit", Value::Null, 200)
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // The buyer checks a balance with the code.
    let b = call(
        &s.client,
        "acme",
        "POST",
        "/api/storefront/gift-cards/balance",
        (None, None),
        json!({ "code": code.to_lowercase() }),
        200,
    )
    .await;
    assert_eq!(b["balance_minor"], 1500);
    call(
        &s.client,
        "acme",
        "POST",
        "/api/storefront/gift-cards/balance",
        (None, None),
        json!({ "code": "AAAA-BBBB-CCCC-DDDD" }),
        404,
    )
    .await;
    // Disabled, it is gone for buyers.
    s.staff(
        "PATCH",
        &format!("/api/gift-cards/{id}"),
        json!({ "active": false }),
        200,
    )
    .await;
    call(
        &s.client,
        "acme",
        "POST",
        "/api/storefront/gift-cards/balance",
        (None, None),
        json!({ "code": code }),
        404,
    )
    .await;

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
    other("GET", format!("/api/gift-cards/{id}"), Value::Null, 404).await;
    other(
        "POST",
        format!("/api/gift-cards/{id}/adjustments"),
        json!({ "amount_minor": 5 }),
        404,
    )
    .await;
    other(
        "PATCH",
        format!("/api/gift-cards/{id}"),
        json!({ "note": "x" }),
        404,
    )
    .await;
    let list = other("GET", "/api/gift-cards".into(), Value::Null, 200).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
    call(
        &s.client,
        "other",
        "POST",
        "/api/storefront/gift-cards/balance",
        (None, None),
        json!({ "code": "XMAS2026ABCD" }),
        404,
    )
    .await;
    call(
        &s.client,
        "acme",
        "GET",
        "/api/gift-cards",
        (None, None),
        Value::Null,
        401,
    )
    .await;
}

#[tokio::test]
async fn gift_cards_pay_at_checkout() {
    let s = shop().await;
    let small = s
        .card(json!({ "amount_minor": 1000, "currency": "EUR" }))
        .await;
    let big = s
        .card(json!({ "amount_minor": 5000, "currency": "EUR" }))
        .await;
    let usd = s
        .card(json!({ "amount_minor": 5000, "currency": "USD" }))
        .await;

    // Part paid by a gift card, the rest by invoice.
    let (path, token) = s.checkout("ada@example.com").await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "gift_card_code": usd["code"] }),
        422,
    )
    .await;
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "gift_card_code": small["code"] }),
            200,
        )
        .await;
    assert_eq!(c["total_minor"], 2875);
    assert_eq!(c["gift_card"]["applied_minor"], 1000);
    assert_eq!(c["amount_due_minor"], 1875);
    let done = s.complete(&path, &token, 201).await;
    assert_eq!(done["payment"]["amount_minor"], 1875);
    assert_eq!(done["order"]["financial_status"], "partially_paid");
    assert_eq!(s.balance(small["id"].as_str().unwrap()).await, 0);

    // A spent card says why at the next checkout.
    let (path, token) = s.checkout("bob@example.com").await;
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "gift_card_code": big["code"] }),
            200,
        )
        .await;
    assert_eq!(c["amount_due_minor"], 0);
    // Nothing left to pay: no payment method needed.
    let done = s
        .shopper("POST", &format!("{path}/complete"), &token, json!({}), 201)
        .await;
    assert!(done["payment"].is_null());
    assert_eq!(done["order"]["financial_status"], "paid");
    let big_id = big["id"].as_str().unwrap();
    assert_eq!(s.balance(big_id).await, 5000 - 2875);
    let order = done["order"]["id"].as_str().unwrap();
    let payments = s
        .staff(
            "GET",
            &format!("/api/payments?reference={order}"),
            Value::Null,
            200,
        )
        .await;
    let pay = &payments["items"][0];
    assert_eq!(pay["provider"], "gift_card");
    assert_eq!(pay["status"], "paid");

    // Refunding it puts the money back on the card, once.
    s.staff(
        "POST",
        &format!("/api/payments/{}/refunds", pay["id"].as_str().unwrap()),
        json!({ "amount_minor": 875 }),
        201,
    )
    .await;
    s.dispatch().await;
    s.dispatch().await;
    assert_eq!(s.balance(big_id).await, 5000 - 2875 + 875);
    let card = s
        .staff(
            "GET",
            &format!("/api/gift-cards/{big_id}"),
            Value::Null,
            200,
        )
        .await;
    assert_eq!(card["transactions"][0]["kind"], "refunded");
    assert_eq!(card["transactions"][1]["kind"], "redeemed");
    assert_eq!(card["transactions"][1]["order_id"], order);
}

#[tokio::test]
async fn customers_spend_their_store_credit() {
    let s = shop().await;
    let me = call(
        &s.client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        (None, None),
        json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        201,
    )
    .await;
    let customer_token = me["token"].as_str().unwrap().to_owned();
    s.card(json!({ "kind": "store_credit", "amount_minor": 1200, "currency": "EUR", "customer_id": me["customer"]["id"] }))
        .await;
    let mine = call(
        &s.client,
        "acme",
        "GET",
        "/api/storefront/account/store-credit",
        (Some(&customer_token), None),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine[0]["balance_minor"], 1200);

    let (path, token) = s.checkout("ada@example.com").await;
    let cart = path.trim_end_matches("/checkout");
    call(
        &s.client,
        "acme",
        "POST",
        &format!("{cart}/customer"),
        (Some(&customer_token), Some(&token)),
        Value::Null,
        200,
    )
    .await;
    let c = s.shopper("GET", &path, &token, Value::Null, 200).await;
    assert_eq!(c["store_credit"]["balance_minor"], 1200);
    assert_eq!(c["store_credit"]["applied_minor"], 0);
    assert_eq!(c["amount_due_minor"], 2875);
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "use_store_credit": true }),
            200,
        )
        .await;
    assert_eq!(c["store_credit"]["applied_minor"], 1200);
    assert_eq!(c["amount_due_minor"], 1675);
    let done = s.complete(&path, &token, 201).await;
    assert_eq!(done["payment"]["amount_minor"], 1675);
    let mine = call(
        &s.client,
        "acme",
        "GET",
        "/api/storefront/account/store-credit",
        (Some(&customer_token), None),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine[0]["balance_minor"], 0);
}

#[tokio::test]
async fn staff_issue_gift_cards_in_the_admin() {
    let s = shop().await;
    s.staff(
        "POST",
        "/api/customers",
        json!({ "email": "ada@example.com" }),
        201,
    )
    .await;
    let res = s
        .client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let post = |path: String, form: Value| {
        s.client
            .post(&path)
            .header("cookie", &cookie)
            .form(&form)
            .send()
    };
    let get = |path: String| s.client.get(&path).header("cookie", &cookie).send();

    assert!(get("/admin/acme/gift-cards".into())
        .await
        .text()
        .contains("No gift cards yet"));
    let res = post(
        "/admin/acme/gift-cards".into(),
        json!({ "kind": "store_credit", "amount": "10" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("give their email"));
    let res = post(
        "/admin/acme/gift-cards".into(),
        json!({ "kind": "gift_card", "amount": "25,00", "note": "Raffle" }),
    )
    .await;
    res.assert_status(201);
    let html = res.text();
    assert!(html.contains("shown only this once"));
    let card = s.staff("GET", "/api/gift-cards", Value::Null, 200).await["items"][0].clone();
    assert_eq!(card["balance_minor"], 2500);
    let page = format!("/admin/acme/gift-cards/{}", card["id"].as_str().unwrap());
    post(
        format!("{page}/adjust"),
        json!({ "amount": "-5", "note": "Fix" }),
    )
    .await
    .assert_status(303);
    let html = get(page.clone()).await.text();
    assert!(html.contains("20.00") && html.contains("Fix"));
    assert!(!html.contains("shown only this once"));
    let res = post(format!("{page}/adjust"), json!({ "amount": "-50" })).await;
    res.assert_status(422);
    post(format!("{page}/disable"), json!({}))
        .await
        .assert_status(303);
    assert!(get(page).await.text().contains("Enable this card"));

    post(
        "/admin/acme/gift-cards".into(),
        json!({ "kind": "store_credit", "amount": "10", "email": "Ada@example.com" }),
    )
    .await
    .assert_status(201);
    let list = get("/admin/acme/gift-cards".into()).await.text();
    assert!(list.contains("Store credit") && list.contains("Disabled"));
}
