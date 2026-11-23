//! D1–D3: the storefront's pages from the theme, and the cart.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn admin(client: &TestClient, t: &str, method: &str, path: &str, body: Value) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    assert!(res.status().is_success(), "{method} {path}: {}", res.text());
    if res.status().as_u16() == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn page(
    client: &TestClient,
    shop: &str,
    path: &str,
    cookie: Option<&str>,
    status: u16,
) -> String {
    let mut req = client.get(path).header("x-soat-tenant", shop);
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let res = req.send().await;
    res.assert_status(status);
    res.text()
}

async fn shop_with_products(client: &TestClient, t: &str) {
    let shoe = admin(
        client,
        t,
        "POST",
        "/api/products",
        json!({
            "sku": "SHOE", "title": "Trail Shoe", "price_minor": 12999, "currency": "EUR",
            "description": "Light and *grippy*. <script>alert(1)</script>"
        }),
    )
    .await;
    let loc = admin(
        client,
        t,
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
    )
    .await;
    admin(
        client,
        t,
        "POST",
        &format!(
            "/api/inventory/{}/adjustments",
            shoe["id"].as_str().unwrap()
        ),
        json!({ "location_id": loc["id"], "delta": 10, "reason": "received" }),
    )
    .await;
    admin(
        client,
        t,
        "POST",
        "/api/products",
        json!({ "sku": "HAT", "title": "Wool Hat", "price_minor": 2500, "currency": "EUR", "status": "draft" }),
    )
    .await;
    admin(
        client,
        t,
        "POST",
        "/api/menus",
        json!({ "title": "Main", "items": [{ "title": "About us", "url": "/pages/about" }] }),
    )
    .await;
}

#[tokio::test]
async fn buyers_browse_the_theme() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    shop_with_products(&client, &t).await;
    let shoe = admin(&client, &t, "GET", "/api/products?q=trail", Value::Null).await;
    let shoe_id = shoe["items"][0]["id"].as_str().unwrap().to_owned();
    admin(&client, &t, "POST", "/api/pages", json!({
        "title": "Home", "published": true, "seo_description": "The best shoes",
        "blocks": [
            { "type": "heading", "text": "Walk further" },
            { "type": "products", "title": "Picked for you", "product_ids": [shoe_id] },
            { "type": "columns", "columns": [[{ "type": "text", "body": "**Free** returns" }], []] }
        ]
    })).await;
    admin(
        &client,
        &t,
        "POST",
        "/api/pages",
        json!({ "title": "About", "published": true,
        "blocks": [{ "type": "text", "body": "We are a small shop." }] }),
    )
    .await;
    let col = admin(
        &client,
        &t,
        "POST",
        "/api/collections",
        json!({ "title": "Shoes" }),
    )
    .await;
    let col_id = col["id"].as_str().unwrap();
    client
        .put(&format!("/api/collections/{col_id}/products/{shoe_id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .assert_status(204);

    let home = page(&client, "acme", "/", None, 200).await;
    assert!(home.starts_with("<!doctype html>"), "{home}");
    assert!(home.contains("<title>acme · acme</title>"), "{home}");
    assert!(home.contains("<meta name=\"description\" content=\"The best shoes\">"));
    assert!(home.contains("Walk further"));
    assert!(home.contains("Picked for you"));
    assert!(home.contains("href=\"/products/trail-shoe\""));
    assert!(home.contains("129.99 EUR"));
    assert!(home.contains("<strong>Free</strong> returns"));
    assert!(home.contains("href=\"/pages/about\""), "the main menu");
    assert!(!home.contains("Wool Hat"), "drafts stay hidden");

    let product = page(&client, "acme", "/products/trail-shoe", None, 200).await;
    assert!(product.contains("<h1>Trail Shoe</h1>"));
    assert!(product.contains("<em>grippy</em>"));
    assert!(product.contains("&lt;script&gt;"));
    assert!(!product.contains("<script>alert"));
    assert!(product.contains("<link rel=\"canonical\" href=\"/products/trail-shoe\">"));
    assert!(product.contains(&format!("value=\"{shoe_id}\"")));

    let missing = page(&client, "acme", "/products/wool-hat", None, 404).await;
    assert!(missing.contains("Not found"));
    page(&client, "globex", "/products/trail-shoe", None, 404).await;

    let col = page(&client, "acme", "/collections/shoes", None, 200).await;
    assert!(col.contains("<h1>Shoes</h1>") && col.contains("Trail Shoe"));
    let found = page(&client, "acme", "/search?q=trail", None, 200).await;
    assert!(found.contains("Results for “trail”") && found.contains("Trail Shoe"));
    let none = page(&client, "acme", "/search?q=boots", None, 200).await;
    assert!(none.contains("Nothing here yet."));
    let about = page(&client, "acme", "/pages/about", None, 200).await;
    assert!(about.contains("We are a small shop."));

    let css = client
        .get("/theme.css")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    css.assert_status(200);
    assert_eq!(css.header("content-type"), Some("text/css; charset=utf-8"));

    // A shop's own template and settings show at once.
    admin(
        &client,
        &t,
        "PUT",
        "/api/theme/files/not_found",
        json!({ "source": "<h1>Gone fishing</h1>" }),
    )
    .await;
    admin(
        &client,
        &t,
        "PUT",
        "/api/theme/settings",
        json!({ "announcement": "Free shipping", "accent_color": "#0a7" }),
    )
    .await;
    let missing = page(&client, "acme", "/pages/nope", None, 404).await;
    assert!(missing.contains("Gone fishing"));
    assert!(missing.contains("Free shipping") && missing.contains("--accent: #0a7"));
    let theirs = page(&client, "globex", "/pages/nope", None, 404).await;
    assert!(!theirs.contains("Gone fishing"));
}

#[tokio::test]
async fn the_cart_lives_in_a_cookie() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    shop_with_products(&client, &t).await;
    let found = admin(&client, &t, "GET", "/api/products", Value::Null).await;
    let id_of = |sku: &str| {
        found["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["sku"] == sku)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (shoe, hat) = (id_of("SHOE"), id_of("HAT"));

    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "product_id": shoe, "quantity": "2" }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/cart"));
    let set = res.header("set-cookie").unwrap().to_owned();
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );
    let cookie = set.split(';').next().unwrap().to_owned();

    let cart = page(&client, "acme", "/cart", Some(&cookie), 200).await;
    assert!(cart.contains("Trail Shoe"), "{cart}");
    assert!(cart.contains("259.98 EUR"));
    assert!(cart.contains("Cart (2)"));

    // htmx gets the cart link back.
    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .header("cookie", &cookie)
        .header("hx-request", "true")
        .form(&json!({ "product_id": shoe }))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("Cart (3)"));
    assert!(res.header("set-cookie").is_none());

    // Drafts cannot be bought; the cookie means nothing in another shop.
    client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .header("cookie", &cookie)
        .form(&json!({ "product_id": hat }))
        .send()
        .await
        .assert_status(422);
    let theirs = page(&client, "globex", "/cart", Some(&cookie), 200).await;
    assert!(theirs.contains("Your cart is empty"));

    // Change and remove lines.
    let line = cart
        .split("/cart/lines/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    let res = client
        .post(&format!("/cart/lines/{line}"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &cookie)
        .header("hx-request", "true")
        .form(&json!({ "quantity": "1" }))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("129.99 EUR"));
    client
        .post(&format!("/cart/lines/{line}"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &cookie)
        .form(&json!({ "quantity": "0" }))
        .send()
        .await
        .assert_status(303);
    let cart = page(&client, "acme", "/cart", Some(&cookie), 200).await;
    assert!(cart.contains("Your cart is empty"));

    // A forged cookie holds no cart.
    let forged = format!("{}.forged", cookie.split('.').next().unwrap());
    let cart = page(&client, "acme", "/cart", Some(&forged), 200).await;
    assert!(cart.contains("Your cart is empty"));
}

async fn form(
    client: &TestClient,
    shop: &str,
    path: &str,
    cookie: &str,
    body: Value,
    status: u16,
) -> zapline::test::TestResponse {
    let res = client
        .post(path)
        .header("x-soat-tenant", shop)
        .header("cookie", cookie)
        .form(&body)
        .send()
        .await;
    res.assert_status(status);
    res
}

#[tokio::test]
async fn buyers_check_out_and_see_their_order() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    shop_with_products(&client, &t).await;
    let z = admin(
        &client,
        &t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": "DE", "countries": ["DE", "AT"] }),
    )
    .await;
    for (name, price) in [("Express", 1495), ("Standard", 495)] {
        admin(
            &client,
            &t,
            "POST",
            &format!("/api/shipping/zones/{}/rates", z["id"].as_str().unwrap()),
            json!({ "name": name, "currency": "EUR", "price_minor": price, "min_days": 1, "max_days": 3 }),
        )
        .await;
    }
    let invoice = admin(
        &client,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
    )
    .await;
    let shoe = admin(&client, &t, "GET", "/api/products?q=trail", Value::Null).await["items"][0]
        ["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Nothing to check out yet.
    let res = client
        .get("/checkout")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/cart"));

    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "product_id": shoe }))
        .send()
        .await;
    let cart = format!("soat_cart={}", res.cookie("soat_cart").unwrap().value());

    let page1 = page(&client, "acme", "/checkout", Some(&cart), 200).await;
    assert!(
        page1.contains("<option value=\"DE\">Germany</option>"),
        "{page1}"
    );
    assert!(page1.contains("<option value=\"AT\">Austria</option>"));
    assert!(!page1.contains("Place order"), "not ready yet");

    let address = json!({
        "email": "ada@example.com", "first_name": "Ada", "last_name": "L",
        "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE",
        "discount_code": ""
    });
    let res = form(&client, "acme", "/checkout", &cart, address.clone(), 303).await;
    assert_eq!(res.header("location"), Some("/checkout"));
    let page2 = page(&client, "acme", "/checkout", Some(&cart), 200).await;
    assert!(page2.contains("value=\"ada@example.com\""), "{page2}");
    assert!(page2.contains("<option value=\"DE\" selected>Germany</option>"));
    // The cheapest option is chosen for the buyer.
    assert!(page2.contains("checked> Standard · 4.95 EUR"), "{page2}");
    assert!(page2.contains("(1–3 days)"));
    assert!(page2.contains("Place order · 134.94 EUR"), "{page2}");
    assert!(page2.contains(&format!(
        "value=\"{}\" checked",
        invoice["id"].as_str().unwrap()
    )));

    // A code that does not apply is shown; the rest is kept.
    let mut bad = address.clone();
    bad["discount_code"] = json!("NOPE");
    let res = form(&client, "acme", "/checkout", &cart, bad, 422).await;
    let text = res.text();
    assert!(text.contains("role=\"alert\""), "{text}");
    assert!(text.contains("value=\"NOPE\""));
    // A bad address is shown back as typed.
    let mut bad = address.clone();
    bad["line1"] = json!("");
    bad["first_name"] = json!("Grace");
    let res = form(&client, "acme", "/checkout", &cart, bad, 422).await;
    assert!(res.text().contains("value=\"Grace\""));
    page(&client, "acme", "/checkout", Some(&cart), 200)
        .await
        .contains("value=\"Ada\"")
        .then_some(())
        .expect("the saved address stays");

    // Express instead.
    let mut express = address.clone();
    let express_id = page2
        .split("name=\"shipping_rate_id\" value=\"")
        .nth(2)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    express["shipping_rate_id"] = json!(express_id);
    form(&client, "acme", "/checkout", &cart, express, 303).await;
    let page3 = page(&client, "acme", "/checkout", Some(&cart), 200).await;
    assert!(page3.contains("checked> Express · 14.95 EUR"), "{page3}");

    // Another shop's method is no method.
    form(
        &client,
        "acme",
        "/checkout/complete",
        &cart,
        json!({ "payment_method_id": "pm_nope" }),
        422,
    )
    .await;
    let res = form(
        &client,
        "acme",
        "/checkout/complete",
        &cart,
        json!({ "payment_method_id": invoice["id"] }),
        303,
    )
    .await;
    assert_eq!(res.header("location"), Some("/order"));
    assert!(res.cookie("soat_cart").unwrap().is_removal());
    let set = res.cookie("soat_order").unwrap();
    assert!(set.http_only());
    let order = format!("soat_order={}", set.value());

    let done = page(&client, "acme", "/order", Some(&order), 200).await;
    assert!(done.contains("Order <strong>#1001</strong>"), "{done}");
    assert!(done.contains("Pay in 14 days"));
    assert!(done.contains("1 × Trail Shoe"));
    assert!(done.contains("144.94 EUR"));
    assert!(done.contains("Hauptstr. 1"));

    // The cart is gone; the order is only the buyer's, and only this shop's.
    page(&client, "acme", "/checkout", Some(&cart), 303).await;
    let forged = format!("{}.forged", order.split('.').next().unwrap());
    page(&client, "acme", "/order", Some(&forged), 404).await;
    page(&client, "acme", "/order", None, 404).await;
    page(&client, "globex", "/order", Some(&order), 404).await;
}

fn cookies(res: &zapline::test::TestResponse, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter_map(|n| res.cookie(n).map(|c| format!("{n}={}", c.value())))
        .collect()
}

#[tokio::test]
async fn customers_have_accounts() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    shop_with_products(&client, &t).await;
    let z = admin(
        &client,
        &t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": "DE", "countries": ["DE"] }),
    )
    .await;
    admin(
        &client,
        &t,
        "POST",
        &format!("/api/shipping/zones/{}/rates", z["id"].as_str().unwrap()),
        json!({ "name": "Standard", "currency": "EUR", "price_minor": 495 }),
    )
    .await;
    let invoice = admin(
        &client,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
    )
    .await;
    let shoe = admin(&client, &t, "GET", "/api/products?q=trail", Value::Null).await["items"][0]
        ["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let res = client
        .get("/account")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/account/login"));
    let login = page(&client, "acme", "/account/login", None, 200).await;
    assert!(login.contains("action=\"/account/login\""));

    // A guest's cart comes along when they register.
    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "product_id": shoe }))
        .send()
        .await;
    let cart = cookies(&res, &["soat_cart"]).remove(0);
    let short = form(
        &client,
        "acme",
        "/account/register",
        &cart,
        json!({ "email": "ada@example.com", "password": "short", "first_name": "Ada" }),
        422,
    )
    .await;
    let text = short.text();
    assert!(
        text.contains("The password needs at least 10 characters"),
        "{text}"
    );
    assert!(text.contains("value=\"ada@example.com\""));
    let res = form(
        &client,
        "acme",
        "/account/register",
        &cart,
        json!({ "email": "ada@example.com", "password": "correct horse", "first_name": "Ada" }),
        303,
    )
    .await;
    assert_eq!(res.header("location"), Some("/account"));
    let session = res.cookie("soat_customer").unwrap();
    assert!(session.http_only() && session.same_site() == Some("Lax"));
    let me = format!("soat_customer={}", session.value());
    let both = format!("{cart}; {me}");
    form(
        &client,
        "acme",
        "/account/register",
        "",
        json!({ "email": "ADA@example.com", "password": "correct horse" }),
        422,
    )
    .await
    .assert_contains("An account with this email exists");

    let home = page(&client, "acme", "/", Some(&me), 200).await;
    assert!(home.contains(">Ada</a>"), "the header greets them");
    let checkout = page(&client, "acme", "/checkout", Some(&both), 200).await;
    assert!(checkout.contains("value=\"ada@example.com\""), "{checkout}");
    form(
        &client,
        "acme",
        "/checkout",
        &both,
        json!({ "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" }),
        303,
    )
    .await;
    form(
        &client,
        "acme",
        "/checkout/complete",
        &both,
        json!({ "payment_method_id": invoice["id"] }),
        303,
    )
    .await;

    let account = page(&client, "acme", "/account", Some(&me), 200).await;
    assert!(account.contains("Hello, Ada"));
    assert!(account.contains("#1001"), "{account}");
    assert!(account.contains("Awaiting payment"));
    assert!(account.contains("134.94 EUR"));
    let order_id = account
        .split("/account/orders/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    let order = page(
        &client,
        "acme",
        &format!("/account/orders/{order_id}"),
        Some(&me),
        200,
    )
    .await;
    assert!(order.contains("<h1>Order #1001</h1>"), "{order}");
    assert!(order.contains("Pay in 14 days"));
    let orders = admin(&client, &t, "GET", "/api/orders", Value::Null).await;
    assert!(orders["items"][0]["customer_id"].is_string(), "{orders}");

    // Details change.
    let res = form(
        &client,
        "acme",
        "/account",
        &me,
        json!({ "first_name": " Grace ", "last_name": "H", "phone": "" }),
        303,
    )
    .await;
    assert_eq!(res.header("location"), Some("/account?saved=1"));
    let account = page(&client, "acme", "/account?saved=1", Some(&me), 200).await;
    assert!(account.contains("Hello, Grace") && account.contains("Saved."));

    // Someone else sees none of it; nor does another shop.
    let res = form(
        &client,
        "acme",
        "/account/register",
        "",
        json!({ "email": "eve@example.com", "password": "another secret" }),
        303,
    )
    .await;
    let eve = cookies(&res, &["soat_customer"]).remove(0);
    page(
        &client,
        "acme",
        &format!("/account/orders/{order_id}"),
        Some(&eve),
        404,
    )
    .await;
    assert!(!page(&client, "acme", "/account", Some(&eve), 200)
        .await
        .contains("#1001"));
    page(&client, "globex", "/account", Some(&me), 303).await;

    // Sign out, and back in.
    let res = form(&client, "acme", "/account/logout", &me, json!({}), 303).await;
    assert!(res.cookie("soat_customer").unwrap().is_removal());
    page(&client, "acme", "/account", Some(&me), 303).await;
    let wrong = form(
        &client,
        "acme",
        "/account/login",
        "",
        json!({ "email": "ada@example.com", "password": "wrong password" }),
        401,
    )
    .await;
    assert!(wrong.text().contains("Wrong email or password."));
    let res = form(
        &client,
        "acme",
        "/account/login",
        "",
        json!({ "email": "ada@example.com", "password": "correct horse" }),
        303,
    )
    .await;
    let me = cookies(&res, &["soat_customer"]).remove(0);
    page(&client, "acme", "/account", Some(&me), 200).await;
}
