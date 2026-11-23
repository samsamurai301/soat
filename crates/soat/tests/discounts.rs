//! C13: discount codes, managed by staff and entered at checkout.

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

    async fn code(&self, body: Value) -> Value {
        self.staff("POST", "/api/discounts", body, 201).await
    }

    /// A cart with two tees, an email, a Berlin address and shipping
    /// chosen; returns its checkout path and token.
    async fn checkout(&self, email: &str) -> (String, String) {
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
async fn staff_manage_codes() {
    let s = shop().await;
    let d = s
        .code(json!({ "code": " summer-10 ", "kind": "percentage", "value": 1000 }))
        .await;
    assert_eq!(d["code"], "SUMMER-10");
    assert_eq!(d["active"], true);
    assert_eq!(d["used_count"], 0);
    let id = d["id"].as_str().unwrap();

    s.staff(
        "POST",
        "/api/discounts",
        json!({ "code": "SUMMER-10", "kind": "free_shipping" }),
        409,
    )
    .await;
    for bad in [
        json!({ "code": "X", "kind": "percentage", "value": 10_001 }),
        json!({ "code": "X", "kind": "fixed_amount", "value": 500 }),
        json!({ "code": "X", "kind": "bogo" }),
        json!({ "code": "NO SPACES", "kind": "free_shipping" }),
        json!({ "code": "X", "kind": "free_shipping", "starts_at": "tomorrow" }),
    ] {
        s.staff("POST", "/api/discounts", bad, 422).await;
    }

    let d = s
        .staff(
            "PUT",
            &format!("/api/discounts/{id}"),
            json!({ "code": "SUMMER-10", "kind": "fixed_amount", "value": 500, "currency": "EUR", "active": false }),
            200,
        )
        .await;
    assert_eq!(d["kind"], "fixed_amount");
    assert_eq!(d["currency"], "EUR");
    assert_eq!(d["active"], false);
    s.code(json!({ "code": "SHIP", "kind": "free_shipping" }))
        .await;
    let list = s
        .staff("GET", "/api/discounts?kind=free_shipping", Value::Null, 200)
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["code"], "SHIP");

    s.staff("DELETE", &format!("/api/discounts/{id}"), Value::Null, 204)
        .await;
    s.staff("GET", &format!("/api/discounts/{id}"), Value::Null, 404)
        .await;
    call(
        &s.client,
        "acme",
        "GET",
        "/api/discounts",
        (None, None),
        Value::Null,
        401,
    )
    .await;
}

#[tokio::test]
async fn a_code_takes_its_share_off_the_order() {
    let s = shop().await;
    let d = s
        .code(json!({ "code": "TEN", "kind": "percentage", "value": 1000 }))
        .await;
    let (path, token) = s.checkout("ada@example.com").await;

    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "NOPE" }),
        422,
    )
    .await;
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "discount_code": "ten" }),
            200,
        )
        .await;
    assert_eq!(c["discount"]["code"], "TEN");
    assert_eq!(c["discount"]["goods_minor"], 238);
    assert_eq!(c["subtotal_minor"], 2380);
    assert_eq!(c["discount_minor"], 238);
    // VAT on 21.42 of goods (342) and 4.95 of shipping (79).
    assert_eq!(c["tax_minor"], 421);
    assert_eq!(c["total_minor"], 2380 + 495 - 238);

    let done = s.complete(&path, &token, 201).await;
    let order = &done["order"];
    assert_eq!(order["discount_code"], "TEN");
    assert_eq!(order["discount_minor"], 238);
    assert_eq!(order["total_minor"], 2637);
    assert_eq!(order["lines"][0]["discount_minor"], 238);
    assert_eq!(order["lines"][0]["tax_minor"], 342);
    assert_eq!(done["payment"]["amount_minor"], 2637);

    let id = d["id"].as_str().unwrap();
    let d = s
        .staff("GET", &format!("/api/discounts/{id}"), Value::Null, 200)
        .await;
    assert_eq!(d["used_count"], 1);
    let events = s
        .staff(
            "GET",
            "/api/events?kind=discount.redeemed",
            Value::Null,
            200,
        )
        .await;
    assert_eq!(events["items"][0]["payload"]["amount_minor"], 238);

    // Removing a code.
    let (path, token) = s.checkout("bob@example.com").await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "TEN" }),
        200,
    )
    .await;
    let c = s
        .shopper("PATCH", &path, &token, json!({ "discount_code": "" }), 200)
        .await;
    assert!(c["discount"].is_null());
    assert_eq!(c["total_minor"], 2875);
}

#[tokio::test]
async fn free_shipping_and_fixed_amounts() {
    let s = shop().await;
    s.code(json!({ "code": "SHIP", "kind": "free_shipping", "min_subtotal_minor": 2000 }))
        .await;
    s.code(json!({ "code": "FIVE", "kind": "fixed_amount", "value": 500, "currency": "EUR" }))
        .await;
    s.code(json!({ "code": "DOLLARS", "kind": "fixed_amount", "value": 500, "currency": "USD" }))
        .await;

    let (path, token) = s.checkout("ada@example.com").await;
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "discount_code": "SHIP" }),
            200,
        )
        .await;
    assert_eq!(c["discount"]["shipping_minor"], 495);
    assert_eq!(c["discount_minor"], 495);
    assert_eq!(c["total_minor"], 2380);
    // No VAT on free shipping.
    assert_eq!(c["tax_minor"], 380);

    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "discount_code": "FIVE" }),
            200,
        )
        .await;
    assert_eq!(c["total_minor"], 2380 + 495 - 500);
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "DOLLARS" }),
        422,
    )
    .await;

    // Below the minimum once the cart shrinks: the checkout says why and
    // will not complete.
    let (path, token) = s.checkout("bob@example.com").await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "SHIP" }),
        200,
    )
    .await;
    let cart = path.trim_end_matches("/checkout");
    let c = s.shopper("GET", cart, &token, Value::Null, 200).await;
    let line = c["lines"][0]["id"].as_str().unwrap();
    s.shopper(
        "PATCH",
        &format!("{cart}/lines/{line}"),
        &token,
        json!({ "quantity": 1 }),
        200,
    )
    .await;
    let c = s.shopper("GET", &path, &token, Value::Null, 200).await;
    assert!(c["discount"].is_null());
    assert!(c["discount_issue"].as_str().unwrap().contains("at least"));
    assert_eq!(c["discount_minor"], 0);
    s.complete(&path, &token, 422).await;
}

#[tokio::test]
async fn limits_hold() {
    let s = shop().await;
    s.code(json!({ "code": "ONCE", "kind": "percentage", "value": 500, "usage_limit": 1 }))
        .await;
    s.code(
        json!({ "code": "MINE", "kind": "percentage", "value": 500, "once_per_customer": true }),
    )
    .await;
    s.code(json!({ "code": "LATER", "kind": "percentage", "value": 500, "starts_at": "2999-01-01T00:00:00Z" }))
        .await;
    s.code(json!({ "code": "OFF", "kind": "percentage", "value": 500, "active": false }))
        .await;

    let (path, token) = s.checkout("ada@example.com").await;
    for code in ["LATER", "OFF"] {
        s.shopper(
            "PATCH",
            &path,
            &token,
            json!({ "discount_code": code }),
            422,
        )
        .await;
    }
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "ONCE" }),
        200,
    )
    .await;
    // A second shopper holds the same code; the first completes.
    let (path2, token2) = s.checkout("bob@example.com").await;
    s.shopper(
        "PATCH",
        &path2,
        &token2,
        json!({ "discount_code": "ONCE" }),
        200,
    )
    .await;
    s.complete(&path, &token, 201).await;
    s.complete(&path2, &token2, 422).await;
    s.shopper(
        "PATCH",
        &path2,
        &token2,
        json!({ "discount_code": "ONCE" }),
        422,
    )
    .await;

    let (path, token) = s.checkout("carol@example.com").await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "MINE" }),
        200,
    )
    .await;
    s.complete(&path, &token, 201).await;
    let (path, token) = s.checkout("Carol@Example.com").await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "discount_code": "MINE" }),
        422,
    )
    .await;
}

#[tokio::test]
async fn codes_stay_in_their_shop() {
    let s = shop().await;
    let d = s
        .code(json!({ "code": "ACME", "kind": "percentage", "value": 1000 }))
        .await;
    let id = d["id"].as_str().unwrap();
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
    other("GET", format!("/api/discounts/{id}"), Value::Null, 404).await;
    other(
        "PUT",
        format!("/api/discounts/{id}"),
        json!({ "code": "ACME", "kind": "free_shipping" }),
        404,
    )
    .await;
    other("DELETE", format!("/api/discounts/{id}"), Value::Null, 404).await;
    let list = other("GET", "/api/discounts".into(), Value::Null, 200).await;
    assert!(list["items"].as_array().unwrap().is_empty());
    // The same code can exist in another shop.
    other(
        "POST",
        "/api/discounts".into(),
        json!({ "code": "ACME", "kind": "free_shipping" }),
        201,
    )
    .await;
    s.staff("GET", &format!("/api/discounts/{id}"), Value::Null, 200)
        .await;
}
