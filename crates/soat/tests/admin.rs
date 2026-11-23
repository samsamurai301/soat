//! E1–E2: the admin in the browser: sign in, pick a shop, manage products.

mod common;

use serde_json::json;
use zapline::test::{TestClient, TestResponse};

async fn sign_in(client: &TestClient, email: &str, password: &str) -> String {
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": email, "password": password }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/admin"));
    let c = res.cookie("soat_admin").unwrap();
    assert!(c.http_only());
    assert_eq!(c.same_site(), Some("Strict"));
    assert_eq!(c.path(), Some("/admin"));
    format!("soat_admin={}", c.value())
}

async fn get(client: &TestClient, cookie: &str, path: &str) -> TestResponse {
    client.get(path).header("cookie", cookie).send().await
}

async fn post(
    client: &TestClient,
    cookie: &str,
    path: &str,
    form: serde_json::Value,
) -> TestResponse {
    client
        .post(path)
        .header("cookie", cookie)
        .form(&form)
        .send()
        .await
}

#[tokio::test]
async fn staff_sign_in_and_pick_a_shop() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let res = client.get("/admin").send().await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/admin/login"));
    let res = client.get("/admin/login").send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
    assert_eq!(res.header("cache-control"), Some("no-store"));

    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": "wrong password!" }))
        .send()
        .await;
    res.assert_status(401);
    assert!(res.text().contains("Wrong email or password."));
    assert!(res.cookie("soat_admin").is_none());

    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let res = get(&client, &admin, "/admin").await;
    res.assert_status(200);
    let html = res.text();
    assert!(html.contains("href=\"/admin/acme\"") && html.contains("href=\"/admin/globex\""));

    let res = get(&client, &admin, "/admin/acme").await;
    res.assert_status(200);
    assert!(res.text().contains("<strong>0</strong> products"));

    // A staff member of one shop goes straight to it and cannot open
    // another.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "email": "sam@example.com", "password": "a long password" }))
        .send()
        .await
        .assert_status(201);
    let sam = sign_in(&client, "sam@example.com", "a long password").await;
    let res = get(&client, &sam, "/admin").await;
    assert_eq!(res.header("location"), Some("/admin/acme"));
    get(&client, &sam, "/admin/globex").await.assert_status(404);
    get(&client, &sam, "/admin/globex/products")
        .await
        .assert_status(404);
    get(&client, &sam, "/admin/acme/products")
        .await
        .assert_status(200);

    // Signing out ends the session.
    let res = post(&client, &sam, "/admin/logout", json!({})).await;
    assert_eq!(res.header("location"), Some("/admin/login"));
    assert!(res.cookie("soat_admin").unwrap().is_removal());
    let res = get(&client, &sam, "/admin/acme").await;
    assert_eq!(res.header("location"), Some("/admin/login"));
}

#[tokio::test]
async fn staff_manage_products() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;

    let res = get(&client, &admin, "/admin/acme/products/new").await;
    res.assert_status(200);
    assert!(res.text().contains("name=\"currency\" value=\"EUR\""));

    // Problems show the form again, as typed.
    let res = post(
        &client,
        &admin,
        "/admin/acme/products",
        json!({ "title": "Trail Shoe", "sku": "SHOE", "price": "12,99", "currency": "EUR" }),
    )
    .await;
    res.assert_status(422);
    let html = res.text();
    assert!(html.contains("The price is an amount in EUR"), "{html}");
    assert!(html.contains("value=\"Trail Shoe\""));

    let res = post(
        &client,
        &admin,
        "/admin/acme/products",
        json!({
            "title": "Trail Shoe", "sku": "SHOE", "price": "129.90", "currency": "EUR",
            "status": "active", "tags": "Running, trail", "description": "Light."
        }),
    )
    .await;
    res.assert_status(303);
    let location = res.header("location").unwrap().to_owned();
    assert!(location.starts_with("/admin/acme/products/prod_") && location.ends_with("?saved=1"));
    let id = location
        .trim_start_matches("/admin/acme/products/")
        .trim_end_matches("?saved=1")
        .to_owned();

    // The API agrees, and the event log names who did it.
    let p: serde_json::Value = client
        .get(&format!("/api/products/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(p["price_minor"], 12990);
    assert_eq!(p["tags"], json!(["running", "trail"]));
    assert_eq!(p["status"], "active");

    let html = get(&client, &admin, &location).await.text();
    assert!(html.contains("Saved.") && html.contains("value=\"129.90\""));

    // A taken SKU is shown, not lost.
    let res = post(
        &client,
        &admin,
        "/admin/acme/products",
        json!({ "title": "Other", "sku": "SHOE", "price": "1", "currency": "EUR" }),
    )
    .await;
    res.assert_status(409);
    assert!(res.text().contains("value=\"Other\""));

    let html = get(&client, &admin, "/admin/acme/products?q=trail")
        .await
        .text();
    assert!(html.contains("Trail Shoe") && html.contains("129.90 EUR"));
    let html = get(&client, &admin, "/admin/acme/products?q=nothing")
        .await
        .text();
    assert!(html.contains("No products match"));
    let html = get(&client, &admin, "/admin/acme/products?status=draft")
        .await
        .text();
    assert!(!html.contains("Trail Shoe"));

    let edit = format!("/admin/acme/products/{id}");
    let res = post(
        &client,
        &admin,
        &edit,
        json!({ "title": "Trail Shoe 2", "sku": "SHOE", "price": "99", "status": "draft" }),
    )
    .await;
    assert_eq!(
        res.header("location"),
        Some(format!("{edit}?saved=1").as_str())
    );
    let html = get(&client, &admin, &edit).await.text();
    assert!(html.contains("value=\"Trail Shoe 2\"") && html.contains("value=\"99.00\""));

    // Another shop's product is not found there.
    get(&client, &admin, &format!("/admin/globex/products/{id}"))
        .await
        .assert_status(404);

    // Forms posted from another site are refused.
    let res = client
        .post(&format!("{edit}/delete"))
        .header("cookie", &admin)
        .header("origin", "https://evil.example")
        .send()
        .await;
    res.assert_status(403);

    let res = post(&client, &admin, &format!("{edit}/delete"), json!({})).await;
    assert_eq!(res.header("location"), Some("/admin/acme/products"));
    get(&client, &admin, &edit).await.assert_status(404);

    let events: serde_json::Value = client
        .get("/api/events?kind=product.deleted")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json();
    assert!(
        events["items"][0]["actor"]
            .as_str()
            .is_some_and(|a| a.starts_with("user_")),
        "{events}"
    );
}

#[tokio::test]
async fn roles_limit_what_the_admin_shows() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    client
        .put("/api/roles/viewer")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "permissions": ["products:read"] }))
        .send()
        .await
        .assert_status(200);
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "email": "viewer@example.com", "password": "a long password", "role": "viewer" }))
        .send()
        .await
        .assert_status(201);
    let viewer = sign_in(&client, "viewer@example.com", "a long password").await;
    get(&client, &viewer, "/admin/acme/products")
        .await
        .assert_status(200);
    let res = post(
        &client,
        &viewer,
        "/admin/acme/products",
        json!({ "title": "X", "sku": "X", "price": "1", "currency": "EUR" }),
    )
    .await;
    res.assert_status(403);
    assert!(res.text().contains("Your role does not allow this."));
}

struct In(&'static str);

impl soat_tenancy::Scope for In {
    fn tenant_id(&self) -> &str {
        self.0
    }
    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// An order of two tees, placed directly.
async fn placed(url: &str, item: &str) -> String {
    use soat_orders::{place, NewOrder, NewOrderLine, NewShipping, PostalAddress};
    use zapline::db::sqlx::{self, PgPool};
    let pool = PgPool::connect(url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let order = NewOrder {
        cart_id: None,
        customer_id: None,
        email: "ada@example.com".into(),
        currency: soat_core::Currency::parse("EUR").unwrap(),
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
        }],
        shipping: Some(NewShipping {
            rate_id: "shr_x".into(),
            name: "Standard".into(),
            price_minor: 495,
            discount_minor: 0,
            tax_minor: 79,
            tax_rate_bps: 1900,
        }),
        shipping_address: Some(PostalAddress {
            first_name: "Ada".into(),
            line1: "Hauptstr. 1".into(),
            postal_code: "10115".into(),
            city: "Berlin".into(),
            country: "DE".into(),
            ..Default::default()
        }),
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

#[tokio::test]
async fn staff_take_orders_through() {
    let (client, url) = common::client_and_url().await;
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "globex"]).await;
    let api = |method: &str, path: &str, body: serde_json::Value| {
        let req = match method {
            "POST" => client.post(path),
            _ => client.get(path),
        }
        .header("x-soat-tenant", "acme")
        .bearer(&t);
        let path = path.to_owned();
        async move {
            let res = req.json(&body).send().await;
            assert!(res.status().is_success(), "{path}: {}", res.text());
            res.json::<serde_json::Value>()
        }
    };
    let loc = api(
        "POST",
        "/api/locations",
        json!({ "name": "A", "country": "DE" }),
    )
    .await;
    let tee = api(
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 1190, "currency": "EUR" }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    api(
        "POST",
        &format!("/api/inventory/{tee}/adjustments"),
        json!({ "location_id": loc["id"], "delta": 5, "reason": "received" }),
    )
    .await;
    let order = placed(&url, &tee).await;
    let m = api(
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice" }),
    )
    .await;
    let pay = api(
        "POST",
        "/api/payments",
        json!({ "method_id": m["id"], "reference": order, "amount_minor": 2875, "currency": "EUR" }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let html = get(&client, &admin, "/admin/acme/orders?view=unpaid")
        .await
        .text();
    assert!(
        html.contains("#1001") && html.contains("28.75 EUR"),
        "{html}"
    );
    let html = get(&client, &admin, "/admin/acme/orders?view=canceled")
        .await
        .text();
    assert!(!html.contains("#1001"));

    let page = format!("/admin/acme/orders/{order}");
    let html = get(&client, &admin, &page).await.text();
    assert!(
        html.contains("Hauptstr. 1<br>") && html.contains("Germany"),
        "{html}"
    );
    assert!(html.contains("Mark paid") && html.contains("Ship what is left"));
    // Nothing paid, nothing to refund.
    assert!(!html.contains("/refund\""));

    // Another order's payment is not this order's to touch.
    post(
        &client,
        &admin,
        &format!("{page}/payments/pay_nope/mark-paid"),
        json!({}),
    )
    .await
    .assert_status(404);
    let res = post(
        &client,
        &admin,
        &format!("{page}/payments/{pay}/mark-paid"),
        json!({}),
    )
    .await;
    assert_eq!(
        res.header("location"),
        Some(format!("{page}?done=paid").as_str())
    );
    let html = get(&client, &admin, &format!("{page}?done=paid"))
        .await
        .text();
    assert!(
        html.contains("Payment marked paid.") && html.contains("/refund\""),
        "{html}"
    );

    let res = post(
        &client,
        &admin,
        &format!("{page}/ship"),
        json!({ "carrier": "DHL", "tracking_number": "123", "tracking_url": "https://dhl.example/123" }),
    )
    .await;
    assert_eq!(
        res.header("location"),
        Some(format!("{page}?done=shipped").as_str())
    );
    let html = get(&client, &admin, &page).await.text();
    assert!(html.contains("href=\"https://dhl.example/123\"") && html.contains("2 units"));
    assert!(!html.contains("Ship what is left") && !html.contains("Cancel order"));

    // A bad amount and too much are both shown on the page.
    let res = post(
        &client,
        &admin,
        &format!("{page}/refund"),
        json!({ "amount": "abc" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("The amount is a sum in EUR"));
    let res = post(
        &client,
        &admin,
        &format!("{page}/refund"),
        json!({ "amount": "99.00" }),
    )
    .await;
    assert!(res.status().is_client_error());
    assert!(res.text().contains("class=\"error\""));
    let res = post(
        &client,
        &admin,
        &format!("{page}/refund"),
        json!({ "amount": "10.00", "reason": "Scratched" }),
    )
    .await;
    assert_eq!(
        res.header("location"),
        Some(format!("{page}?done=refunded").as_str())
    );
    // The order follows its payments through an event subscriber.
    soat_events::dispatch_resources(&client.resources().await)
        .await
        .unwrap();
    let o = api(
        "GET",
        &format!("/api/orders/{order}"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(o["financial_status"], "partially_refunded");

    // Another shop's admin does not find the order.
    get(&client, &admin, &format!("/admin/globex/orders/{order}"))
        .await
        .assert_status(404);
}
