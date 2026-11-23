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
