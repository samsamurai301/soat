//! C9: checkout from a cart to an order and a payment.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

/// Staff (`bearer`) or a shopper holding a cart token.
#[allow(clippy::too_many_arguments)]
async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    cart: Option<&str>,
    body: Value,
    status: u16,
) -> Value {
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
    let req = match cart {
        Some(t) => req.header("x-soat-cart-token", t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 400 {
        Value::Null
    } else {
        res.json()
    }
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
            None,
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
            None,
            Some(cart),
            body,
            status,
        )
        .await
    }

    async fn product(&self, sku: &str, price: i64, class: &str, units: i64, loc: &Value) -> String {
        let p = self
            .staff(
                "POST",
                "/api/products",
                json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR", "tax_class": class }),
                201,
            )
            .await;
        let id = p["id"].as_str().unwrap().to_owned();
        self.staff(
            "POST",
            &format!("/api/inventory/{id}/adjustments"),
            json!({ "location_id": loc, "delta": units, "reason": "received" }),
            200,
        )
        .await;
        id
    }

    /// A cart with two tees and a book; returns its id and token.
    async fn cart(&self) -> (String, String) {
        let c = call(
            &self.client,
            "acme",
            "POST",
            "/api/storefront/carts",
            None,
            None,
            json!({ "currency": "EUR" }),
            201,
        )
        .await;
        let id = c["cart"]["id"].as_str().unwrap().to_owned();
        let token = c["token"].as_str().unwrap().to_owned();
        for (item, q) in [(&self.tee, 2), (&self.book, 1)] {
            self.shopper(
                "POST",
                &format!("/api/storefront/carts/{id}/lines"),
                &token,
                json!({ "item_id": item, "quantity": q }),
                200,
            )
            .await;
        }
        (id, token)
    }
}

/// A German shop with prices including VAT, EU zones, domestic shipping
/// and invoice payment.
async fn shop() -> Shop {
    let (client, admin) = common::with_tenants(&["acme"]).await;
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
    s.tee = s.product("TEE", 1190, "standard", 5, &loc).await;
    s.book = s.product("BOOK", 1070, "reduced", 1, &loc).await;
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

fn berlin() -> Value {
    json!({ "first_name": "Ada", "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" })
}

#[tokio::test]
async fn a_cart_becomes_a_paid_order() {
    let s = shop().await;
    let (id, token) = s.cart().await;
    let path = format!("/api/storefront/carts/{id}/checkout");

    let c = s.shopper("GET", &path, &token, Value::Null, 200).await;
    assert_eq!(c["missing"], json!(["email", "shipping_address"]));
    assert_eq!(c["subtotal_minor"], 3450);

    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "email": "Ada@Example.com", "shipping_address": berlin() }),
            200,
        )
        .await;
    assert_eq!(c["missing"], json!(["shipping"]));
    assert_eq!(c["shipping_options"][0]["name"], "Standard");
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "shipping_rate_id": "shr_nope" }),
        422,
    )
    .await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "shipping_address": { "line1": "x", "city": "y", "country": "Germany" } }),
        422,
    )
    .await;
    let c = s
        .shopper(
            "PATCH",
            &path,
            &token,
            json!({ "shipping_rate_id": s.rate }),
            200,
        )
        .await;
    assert_eq!(c["ready"], true);
    assert_eq!(c["shipping_minor"], 495);
    // Gross prices: 380 + 70 VAT in the goods, 79 in shipping.
    assert_eq!(c["tax_minor"], 529);
    assert_eq!(c["total_minor"], 3945);
    assert_eq!(c["billing_address"]["city"], "Berlin");

    s.shopper(
        "POST",
        &format!("{path}/complete"),
        &token,
        json!({ "payment_method_id": "pm_nope" }),
        422,
    )
    .await;
    let done = s
        .shopper(
            "POST",
            &format!("{path}/complete"),
            &token,
            json!({ "payment_method_id": s.invoice }),
            201,
        )
        .await;
    let order = &done["order"];
    let order_id = order["id"].as_str().unwrap();
    assert_eq!(order["number"], 1001);
    assert_eq!(order["email"], "ada@example.com");
    assert_eq!(order["total_minor"], 3945);
    assert_eq!(order["lines"][0]["tax_rate_bps"], 1900);
    assert_eq!(order["lines"][1]["tax_minor"], 70);
    assert_eq!(order["tax_country"], "DE");
    assert!(done["order_token"]
        .as_str()
        .unwrap()
        .starts_with("soat_order_"));
    let payment = &done["payment"];
    assert_eq!(payment["status"], "pending");
    assert_eq!(payment["amount_minor"], 3945);
    assert_eq!(payment["next_action"]["text"], "Pay in 14 days");

    // Stock is held for the order, the cart is closed.
    let held = s
        .staff(
            "GET",
            &format!("/api/inventory/reservations?reference={order_id}"),
            Value::Null,
            200,
        )
        .await;
    assert_eq!(held["total"], 2);
    s.shopper(
        "POST",
        &format!("{path}/complete"),
        &token,
        json!({ "payment_method_id": s.invoice }),
        409,
    )
    .await;
    let cart = s
        .shopper(
            "GET",
            &format!("/api/storefront/carts/{id}"),
            &token,
            Value::Null,
            200,
        )
        .await;
    assert_eq!(cart["status"], "completed");

    let pay = payment["id"].as_str().unwrap();
    s.staff(
        "POST",
        &format!("/api/payments/{pay}/mark-paid"),
        Value::Null,
        200,
    )
    .await;
    for _ in 0..3 {
        soat_events::dispatch_resources(&s.client.resources().await)
            .await
            .unwrap();
    }
    let o = s
        .staff("GET", &format!("/api/orders/{order_id}"), Value::Null, 200)
        .await;
    assert_eq!(o["financial_status"], "paid");
    let held = s
        .staff(
            "GET",
            &format!("/api/inventory/reservations?reference={order_id}"),
            Value::Null,
            200,
        )
        .await;
    assert!(held["items"][0]["expires_at"].is_null());
}

#[tokio::test]
async fn incomplete_or_unstocked_checkouts_place_nothing() {
    let s = shop().await;
    let (id, token) = s.cart().await;
    let path = format!("/api/storefront/carts/{id}/checkout");
    s.shopper(
        "POST",
        &format!("{path}/complete"),
        &token,
        json!({ "payment_method_id": s.invoice }),
        422,
    )
    .await;
    s.shopper(
        "PATCH",
        &path,
        &token,
        json!({ "email": "a@example.com", "shipping_address": berlin(), "shipping_rate_id": s.rate }),
        200,
    )
    .await;
    // Someone else takes the last book first.
    s.staff(
        "POST",
        "/api/inventory/reservations",
        json!({ "item_id": s.book, "quantity": 1, "reference": "elsewhere" }),
        201,
    )
    .await;
    let c = s.shopper("GET", &path, &token, Value::Null, 200).await;
    assert_eq!(c["missing"], json!(["lines"]));
    s.shopper(
        "POST",
        &format!("{path}/complete"),
        &token,
        json!({ "payment_method_id": s.invoice }),
        422,
    )
    .await;
    let orders = s.staff("GET", "/api/orders", Value::Null, 200).await;
    assert_eq!(orders["total"], 0);
    let cart = s
        .shopper(
            "GET",
            &format!("/api/storefront/carts/{id}"),
            &token,
            Value::Null,
            200,
        )
        .await;
    assert_eq!(cart["status"], "open");
}

#[tokio::test]
async fn another_shop_cannot_reach_the_checkout() {
    let s = shop().await;
    let (id, token) = s.cart().await;
    common::create_tenants(&s.client, &s.admin, &["globex"]).await;
    call(
        &s.client,
        "globex",
        "GET",
        &format!("/api/storefront/carts/{id}/checkout"),
        None,
        Some(&token),
        Value::Null,
        404,
    )
    .await;
    call(
        &s.client,
        "acme",
        "GET",
        &format!("/api/storefront/carts/{id}/checkout"),
        None,
        Some("soat_cart_wrong"),
        Value::Null,
        404,
    )
    .await;
}
