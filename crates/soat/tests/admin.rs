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
    assert!(res.text().contains("Finish setting up"));

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
    placed_in(url, "acme", item).await
}

async fn placed_in(url: &str, shop: &'static str, item: &str) -> String {
    use soat_orders::{place, NewOrder, NewOrderLine, NewShipping, PostalAddress};
    use zapline::db::sqlx::{self, PgPool};
    let pool = PgPool::connect(url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', $1, true)")
        .bind(shop)
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
    let (o, _) = place(&mut tx, &In(shop), &order).await.unwrap();
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

#[tokio::test]
async fn staff_look_after_customers() {
    let (client, _) = common::with_tenants(&["acme", "globex"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    get(&client, &admin, "/admin/acme/customers/new")
        .await
        .assert_status(200);
    let res = post(
        &client,
        &admin,
        "/admin/acme/customers",
        json!({ "email": "not an email", "first_name": "Ada" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("value=\"Ada\""));
    let res = post(
        &client,
        &admin,
        "/admin/acme/customers",
        json!({
            "email": "ada@example.com", "first_name": "Ada", "last_name": "Lovelace",
            "accepts_marketing": "1", "tags": "VIP, wholesale", "note": "Prefers email"
        }),
    )
    .await;
    res.assert_status(303);
    let location = res.header("location").unwrap().to_owned();
    let page = location.trim_end_matches("?saved=1").to_owned();
    let html = get(&client, &admin, &location).await.text();
    assert!(html.contains("<h1>Ada Lovelace</h1>") && html.contains("value=\"vip, wholesale\""));
    assert!(html.contains("name=\"accepts_marketing\" value=\"1\" checked"));
    assert!(html.contains("No orders yet.") && html.contains("Guest: no account."));

    let res = post(
        &client,
        &admin,
        "/admin/acme/customers",
        json!({ "email": "ada@example.com" }),
    )
    .await;
    res.assert_status(409);

    let html = get(&client, &admin, "/admin/acme/customers?tag=vip")
        .await
        .text();
    assert!(html.contains("ada@example.com"));
    let html = get(&client, &admin, "/admin/acme/customers?q=grace")
        .await
        .text();
    assert!(html.contains("No customers here"));
    let html = get(&client, &admin, "/admin/globex/customers").await.text();
    assert!(!html.contains("ada@example.com"));

    // Unticking the box withdraws consent.
    let res = post(
        &client,
        &admin,
        &page,
        json!({ "email": "ada@example.com", "first_name": "Ada", "last_name": "Byron" }),
    )
    .await;
    assert_eq!(res.header("location"), Some(location.as_str()));
    let html = get(&client, &admin, &page).await.text();
    assert!(html.contains("<h1>Ada Byron</h1>"));
    assert!(html.contains("name=\"accepts_marketing\" value=\"1\">"));

    let id = page.rsplit('/').next().unwrap();
    get(&client, &admin, &format!("/admin/globex/customers/{id}"))
        .await
        .assert_status(404);
    let res = post(&client, &admin, &format!("{page}/delete"), json!({})).await;
    assert_eq!(res.header("location"), Some("/admin/acme/customers"));
    get(&client, &admin, &page).await.assert_status(404);
}

#[tokio::test]
async fn staff_edit_content_and_theme() {
    let (client, _) = common::with_tenants(&["acme"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let shop_page = |path: &'static str| {
        let client = &client;
        async move {
            let res = client
                .get(path)
                .header("x-soat-tenant", "acme")
                .send()
                .await;
            res.text()
        }
    };

    // A page: bad JSON is kept and explained; a good one is published.
    let res = post(
        &client,
        &admin,
        "/admin/acme/pages",
        json!({ "title": "About us", "blocks": "[{ nope" }),
    )
    .await;
    res.assert_status(422);
    let html = res.text();
    assert!(
        html.contains("not valid JSON") && html.contains("[{ nope"),
        "{html}"
    );
    let res = post(
        &client,
        &admin,
        "/admin/acme/pages",
        json!({ "title": "About us", "published": "1",
                "blocks": "[{\"type\":\"text\",\"body\":\"We make *shoes*.\"}]" }),
    )
    .await;
    res.assert_status(303);
    let page = res
        .header("location")
        .unwrap()
        .trim_end_matches("?saved=1")
        .to_owned();
    assert!(shop_page("/pages/about-us")
        .await
        .contains("<em>shoes</em>"));
    let html = get(&client, &admin, "/admin/acme/pages").await.text();
    assert!(html.contains("/pages/about-us") && html.contains("Published"));
    // An unknown block type is the API's 422, shown on the form.
    let res = post(
        &client,
        &admin,
        &page,
        json!({ "title": "About us", "blocks": "[{\"type\":\"script\"}]" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("class=\"error\""));

    // A menu written as lines.
    let res = post(
        &client,
        &admin,
        "/admin/acme/menus",
        json!({ "title": "Main", "handle": "main",
                "items": "Shop | /collections/all\r\n- Shoes | /collections/shoes\r\nAbout | /pages/about-us" }),
    )
    .await;
    res.assert_status(303);
    let menu = res.header("location").unwrap().to_owned();
    let html = get(&client, &admin, &menu).await.text();
    assert!(
        html.contains(
            "Shop | /collections/all\n- Shoes | /collections/shoes\nAbout | /pages/about-us"
        ),
        "{html}"
    );
    let html = shop_page("/").await;
    assert!(html.contains("href=\"/collections/shoes\"") && html.contains(">About</a>"));
    let res = post(
        &client,
        &admin,
        "/admin/acme/menus",
        json!({ "title": "Footer", "items": "-- Deep | /x" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("nested too deep"));

    // Redirects.
    let res = post(
        &client,
        &admin,
        "/admin/acme/redirects",
        json!({ "from_path": "/old-about", "to_url": "/pages/about-us" }),
    )
    .await;
    assert_eq!(res.header("location"), Some("/admin/acme/redirects"));
    let res = client
        .get("/old-about")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(301);
    let res = post(
        &client,
        &admin,
        "/admin/acme/redirects",
        json!({ "from_path": "nope", "to_url": "/x" }),
    )
    .await;
    res.assert_status(422);

    // Theme settings and a file.
    let res = post(
        &client,
        &admin,
        "/admin/acme/theme",
        json!({ "accent_color": "#ff0000", "announcement": "Free shipping over €50" }),
    )
    .await;
    assert_eq!(res.header("location"), Some("/admin/acme/theme?saved=1"));
    assert!(shop_page("/").await.contains("Free shipping over €50"));
    let res = post(
        &client,
        &admin,
        "/admin/acme/theme",
        json!({ "accent_color": "red" }),
    )
    .await;
    res.assert_status(422);
    let html = get(&client, &admin, "/admin/acme/theme/files/footer")
        .await
        .text();
    assert!(html.contains("site-footer"));
    let res = post(
        &client,
        &admin,
        "/admin/acme/theme/files/footer",
        json!({ "source": "<footer>{{#if x }}</footer>" }),
    )
    .await;
    res.assert_status(422);
    let res = post(
        &client,
        &admin,
        "/admin/acme/theme/files/footer",
        json!({ "source": "<footer>Made by hand in {{ shop.name }}</footer>" }),
    )
    .await;
    res.assert_status(303);
    assert!(shop_page("/").await.contains("Made by hand in acme"));
    let res = post(
        &client,
        &admin,
        "/admin/acme/theme/files/footer/reset",
        json!({}),
    )
    .await;
    assert_eq!(res.header("location"), Some("/admin/acme/theme"));
    assert!(!shop_page("/").await.contains("Made by hand"));

    let res = post(&client, &admin, &format!("{page}/delete"), json!({})).await;
    assert_eq!(res.header("location"), Some("/admin/acme/pages"));
    post(&client, &admin, "/admin/acme/nothing/x/delete", json!({}))
        .await
        .assert_status(404);
}

#[tokio::test]
async fn owners_change_settings() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let res = get(&client, &admin, "/admin/acme/settings").await;
    assert_eq!(res.header("location"), Some("/admin/acme/settings/general"));
    get(&client, &admin, "/admin/acme/settings/nothing")
        .await
        .assert_status(404);

    // Languages and domains.
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/general/languages",
        json!({ "default_language": "en", "languages": "de, fr" }),
    )
    .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/acme/settings/general?done=1")
    );
    let html = get(&client, &admin, "/admin/acme/settings/general?done=1")
        .await
        .text();
    assert!(html.contains("value=\"de, fr\"") && html.contains("Saved."));
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/general/languages",
        json!({ "default_language": "english" }),
    )
    .await;
    res.assert_status(422);
    post(
        &client,
        &admin,
        "/admin/acme/settings/general/domains",
        json!({ "domain": "shop.acme.example" }),
    )
    .await
    .assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/general")
        .await
        .text();
    assert!(html.contains("shop.acme.example"));
    post(
        &client,
        &admin,
        "/admin/acme/settings/general/remove-domain/shop.acme.example",
        json!({}),
    )
    .await
    .assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/general")
        .await
        .text();
    assert!(!html.contains("shop.acme.example"));

    // Staff and their roles.
    post(
        &client,
        &admin,
        "/admin/acme/settings/staff/add",
        json!({ "email": "sam@example.com", "password": "a long password", "role": "staff" }),
    )
    .await
    .assert_status(303);
    let members: serde_json::Value = client
        .get("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json();
    let sam = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["email"] == "sam@example.com")
        .unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    post(
        &client,
        &admin,
        &format!("/admin/acme/settings/staff/role/{sam}"),
        json!({ "role": "owner" }),
    )
    .await
    .assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/staff")
        .await
        .text();
    assert!(html.contains("sam@example.com"));
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/staff/add",
        json!({ "email": "sam@example.com", "role": "staff" }),
    )
    .await;
    res.assert_status(409);

    // An API key: its secret is shown once, and it works.
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/developers/keys",
        json!({ "name": "ERP" }),
    )
    .await;
    res.assert_status(201);
    let html = res.text();
    let start = html.find("soat_sk_acme_").expect("the secret is shown");
    let secret: String = html[start..].chars().take_while(|c| *c != '<').collect();
    assert!(html.contains("it is not shown again"));
    client
        .get("/api/products")
        .bearer(&secret)
        .send()
        .await
        .assert_status(403);
    let html = get(&client, &admin, "/admin/acme/settings/developers")
        .await
        .text();
    assert!(!html.contains(&secret), "the secret is not shown again");
    let keys: serde_json::Value = client
        .get("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json();
    let key = keys[0]["id"].as_str().unwrap().to_owned();
    post(
        &client,
        &admin,
        &format!("/admin/acme/settings/developers/revoke-key/{key}"),
        json!({}),
    )
    .await
    .assert_status(303);
    client
        .get("/api/products")
        .bearer(&secret)
        .send()
        .await
        .assert_status(401);

    // Webhooks only go to public HTTPS addresses.
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/developers/webhooks",
        json!({ "url": "http://localhost/x" }),
    )
    .await;
    res.assert_status(422);

    // Claude's changes can run without review.
    post(
        &client,
        &admin,
        "/admin/acme/settings/assistant/approval",
        json!({}),
    )
    .await
    .assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/assistant")
        .await
        .text();
    assert!(html.contains("name=\"review\" value=\"1\">") && html.contains("Nothing waiting."));
    let s: serde_json::Value = client
        .get("/api/assistant/settings")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json();
    assert_eq!(s["approval"], "off");
    // Another shop is untouched.
    let s: serde_json::Value = client
        .get("/api/assistant/settings")
        .header("x-soat-tenant", "globex")
        .bearer(&t)
        .send()
        .await
        .json();
    assert_eq!(s["approval"], "writes");
}

#[tokio::test]
async fn owners_set_up_selling() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let api = |path: &'static str| {
        let (client, t) = (&client, &t);
        async move {
            client
                .get(path)
                .header("x-soat-tenant", "acme")
                .bearer(t)
                .send()
                .await
                .json::<serde_json::Value>()
        }
    };
    for area in ["payments", "shipping", "taxes", "email", "invoices"] {
        get(&client, &admin, &format!("/admin/acme/settings/{area}"))
            .await
            .assert_status(200);
    }

    // Payments: a manual method with instructions, off and on again.
    post(
        &client,
        &admin,
        "/admin/acme/settings/payments/add",
        json!({ "provider": "manual", "name": "Bank transfer", "instructions": "IBAN DE00 …" }),
    )
    .await
    .assert_status(303);
    let methods = api("/api/payment-methods").await;
    let m = methods["items"][0].clone();
    assert_eq!(m["settings"]["instructions"], "IBAN DE00 …");
    let id = m["id"].as_str().unwrap();
    post(
        &client,
        &admin,
        &format!("/admin/acme/settings/payments/disable/{id}"),
        json!({}),
    )
    .await
    .assert_status(303);
    assert_eq!(
        api("/api/payment-methods").await["items"][0]["active"],
        false
    );

    // Shipping: a zone and a rate that is free from 50.
    post(
        &client,
        &admin,
        "/admin/acme/settings/shipping/add-zone",
        json!({ "name": "Germany", "countries": "de" }),
    )
    .await
    .assert_status(303);
    let zone = api("/api/shipping/zones").await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = post(
        &client,
        &admin,
        &format!("/admin/acme/settings/shipping/add-rate/{zone}"),
        json!({ "name": "Standard", "currency": "EUR", "price": "4,95" }),
    )
    .await;
    res.assert_status(303);
    let res = post(
        &client,
        &admin,
        &format!("/admin/acme/settings/shipping/add-rate/{zone}"),
        json!({ "name": "Express", "currency": "EUR", "price": "9.90", "free_from": "50", "min_days": "1", "max_days": "2" }),
    )
    .await;
    res.assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/shipping")
        .await
        .text();
    assert!(
        html.contains("4.95 EUR") && html.contains("from 50.00 EUR") && html.contains("1–2 days"),
        "{html}"
    );
    let res = post(
        &client,
        &admin,
        &format!("/admin/acme/settings/shipping/add-rate/{zone}"),
        json!({ "name": "Bad", "currency": "EUR", "price": "free" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("The price is an amount in EUR"));
    let quote: serde_json::Value = client
        .post("/api/shipping/options")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "country": "DE", "currency": "EUR", "subtotal_minor": 6000, "weight_grams": 0 }))
        .send()
        .await
        .json();
    let prices: Vec<i64> = quote["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["price_minor"].as_i64().unwrap())
        .collect();
    assert_eq!(prices, vec![0, 495], "{quote}");

    // Taxes: settings, the EU preset and a zone of its own.
    post(
        &client,
        &admin,
        "/admin/acme/settings/taxes/settings",
        json!({ "origin_country": "de", "prices_include_tax": "1" }),
    )
    .await
    .assert_status(303);
    let tax = api("/api/tax/settings").await;
    assert_eq!(tax["origin_country"], "DE");
    assert_eq!(tax["charge_destination_vat"], false);
    post(
        &client,
        &admin,
        "/admin/acme/settings/taxes/eu-vat",
        json!({}),
    )
    .await
    .assert_status(303);
    post(
        &client,
        &admin,
        "/admin/acme/settings/taxes/add-zone",
        json!({ "name": "Switzerland", "countries": "CH, li", "standard": "8,1", "reduced": "2.6" }),
    )
    .await
    .assert_status(303);
    let html = get(&client, &admin, "/admin/acme/settings/taxes")
        .await
        .text();
    assert!(
        html.contains("reduced 2.6%, standard 8.1%") && html.contains("standard 19%"),
        "{html}"
    );
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/taxes/add-zone",
        json!({ "name": "X", "countries": "NO", "standard": "lots" }),
    )
    .await;
    res.assert_status(422);

    // Email: sender, a template off, a template reworded.
    post(
        &client,
        &admin,
        "/admin/acme/settings/email/settings",
        json!({ "sender_name": "Acme", "reply_to": "help@acme.example", "enabled": "order_confirmation" }),
    )
    .await
    .assert_status(303);
    let n = api("/api/notifications/settings").await;
    assert_eq!(n["sender_name"], "Acme");
    let off = n["disabled"].as_array().unwrap();
    assert!(off.contains(&json!("order_canceled")) && !off.contains(&json!("order_confirmation")));
    let res = post(
        &client,
        &admin,
        "/admin/acme/emails/order_confirmation",
        json!({ "subject": "Thanks {{#if }}", "text": "x" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("value=\"Thanks {{#if }}\""));
    let res = post(
        &client,
        &admin,
        "/admin/acme/emails/order_confirmation",
        json!({ "subject": "Danke für Bestellung {{ order.number }}", "text": "Hallo\r\nDanke!" }),
    )
    .await;
    res.assert_status(303);
    let tpl = api("/api/notifications/templates/order_confirmation").await;
    assert_eq!(tpl["text"], "Hallo\nDanke!");
    assert_eq!(tpl["customized"], true);
    post(
        &client,
        &admin,
        "/admin/acme/emails/order_confirmation/reset",
        json!({}),
    )
    .await
    .assert_status(303);
    assert_eq!(
        api("/api/notifications/templates/order_confirmation").await["customized"],
        false
    );

    // Invoices: legal details.
    let res = post(
        &client,
        &admin,
        "/admin/acme/settings/invoices/settings",
        json!({
            "legal_name": "Acme GmbH", "line1": "Hauptstr. 1", "postal_code": "10115",
            "city": "Berlin", "country": "de", "vat_number": "DE123456789", "issue_on": "placed"
        }),
    )
    .await;
    res.assert_status(303);
    let inv = api("/api/invoices/settings").await;
    assert_eq!(inv["legal_name"], "Acme GmbH");
    assert_eq!(inv["issue_on"], "placed");
    let html = get(&client, &admin, "/admin/acme/settings/invoices")
        .await
        .text();
    assert!(html.contains("value=\"placed\" selected"));
}

#[tokio::test]
async fn the_dashboard_shows_what_needs_doing() {
    let (client, url) = common::client_and_url().await;
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "globex"]).await;
    let admin = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    let html = get(&client, &admin, "/admin/acme").await.text();
    assert!(
        html.contains("Finish setting up") && html.contains("No orders in the last 30 days."),
        "{html}"
    );

    let report = |path: &'static str| {
        let req = client.get(path).header("x-soat-tenant", "acme").bearer(&t);
        async move { req.send().await }
    };
    report("/api/orders/report?days=0").await.assert_status(422);
    report("/api/orders/report?days=367")
        .await
        .assert_status(422);

    let kept = placed(&url, "prd_tee").await;
    let gone = placed(&url, "prd_tee").await;
    placed_in(&url, "globex", "prd_other").await;
    client
        .post(&format!("/api/orders/{gone}/cancel"))
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    let res = report("/api/orders/report").await;
    res.assert_status(200);
    let r = res.json::<serde_json::Value>();
    assert_eq!(r["days"], 30);
    // The canceled order and globex's are left out.
    assert_eq!(
        r["sales"],
        json!([{ "currency": "EUR", "orders": 1, "total_minor": 2875, "average_minor": 2875 }])
    );
    assert_eq!(r["by_day"].as_array().unwrap().len(), 1);
    assert_eq!(r["top_products"][0]["product_id"], "prd_tee");
    assert_eq!(r["top_products"][0]["quantity"], 2);
    assert_eq!(r["to_do"], json!({ "unpaid": 1, "unfulfilled": 1 }));
    assert!(!kept.is_empty());

    let html = get(&client, &admin, "/admin/acme?days=7").await.text();
    assert!(
        html.contains("28.75 EUR") && html.contains("from 1 orders"),
        "{html}"
    );
    assert!(html.contains("orders waiting for payment") && html.contains("orders to ship"));
    assert!(html.contains("<rect") && html.contains("Best sellers"));
    assert!(html.contains("?days=7\" aria-current=\"page\""), "{html}");

    // Staff who may not see orders get the page without sales.
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
    let res = get(&client, &viewer, "/admin/acme").await;
    res.assert_status(200);
    let html = res.text();
    assert!(!html.contains("Sales") && !html.contains("28.75"), "{html}");
}

#[tokio::test]
async fn the_palette_finds_pages_and_records() {
    let (client, url) = common::client_and_url().await;
    let admin = common::setup_admin(&client).await;
    common::create_tenants(&client, &admin, &["acme"]).await;
    let product: serde_json::Value = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "sku": "TEE", "title": "Organic tee", "price_minor": 1190, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    let id = product["id"].as_str().unwrap();
    placed(&url, id).await;
    let cookie = sign_in(&client, common::ADMIN_EMAIL, common::ADMIN_PASSWORD).await;

    let home = get(&client, &cookie, "/admin/acme").await.text();
    assert!(home.contains("id=\"palette\""));
    assert!(home.contains("hx-get=\"/admin/acme/palette\""));

    let html = get(&client, &cookie, "/admin/acme/palette?q=vat")
        .await
        .text();
    assert!(
        html.contains("href=\"/admin/acme/settings/taxes\""),
        "{html}"
    );
    assert!(!html.contains("<html"));
    let html = get(&client, &cookie, "/admin/acme/palette?q=organic")
        .await
        .text();
    assert!(html.contains(&format!("href=\"/admin/acme/products/{id}\"")));
    let html = get(&client, &cookie, "/admin/acme/palette?q=%231001")
        .await
        .text();
    assert!(html.contains("#1001"), "{html}");
    assert!(html.contains("ada@example.com"));
    let html = get(&client, &cookie, "/admin/acme/palette?q=zzzz")
        .await
        .text();
    assert!(html.contains("Nothing matches."));
    // Signed out: to sign-in, not results.
    let res = client.get("/admin/acme/palette?q=tee").send().await;
    assert_eq!(res.header("location"), Some("/admin/login"));
}
