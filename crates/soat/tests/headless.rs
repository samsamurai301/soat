//! D11: the storefront as JSON for a front end built elsewhere, in the
//! buyer's language and market, callable from any origin.

mod common;

use serde_json::{json, Value};
use zapline::http::Method;
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    t: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    res.json()
}

async fn public(client: &TestClient, shop: &str, path: &str, status: u16) -> Value {
    let res = client
        .get(path)
        .header("x-soat-tenant", shop)
        .header("origin", "https://shop.example.com")
        .send()
        .await;
    res.assert_status(status);
    assert_eq!(res.header("access-control-allow-origin"), Some("*"));
    res.json()
}

#[tokio::test]
async fn front_ends_read_the_shop_in_language_and_market() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    send(
        &client,
        &t,
        "PUT",
        "/api/languages",
        json!({ "default_language": "en", "languages": ["de"] }),
        200,
    )
    .await;
    let p = send(
        &client,
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "SHOE", "title": "Trail Shoe", "price_minor": 2000,
                "currency": "EUR", "status": "active" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap().to_owned();
    let c = send(
        &client,
        &t,
        "POST",
        "/api/collections",
        json!({ "title": "Shoes" }),
        201,
    )
    .await;
    let page = send(
        &client,
        &t,
        "POST",
        "/api/pages",
        json!({ "title": "About", "published": true,
                "blocks": [{ "type": "text", "body": "Hello" }] }),
        201,
    )
    .await;
    let menu = send(
        &client,
        &t,
        "POST",
        "/api/menus",
        json!({ "title": "Main", "items": [{ "title": "About us", "url": "/pages/about" }] }),
        201,
    )
    .await;
    for (kind, rid, fields) in [
        ("product", id.as_str(), json!({ "title": "Trailschuh" })),
        (
            "collection",
            c["id"].as_str().unwrap(),
            json!({ "title": "Schuhe" }),
        ),
        (
            "page",
            page["id"].as_str().unwrap(),
            json!({ "title": "Über" }),
        ),
        (
            "menu",
            menu["id"].as_str().unwrap(),
            json!({ "items": "[{\"title\":\"Über uns\",\"url\":\"/pages/about\"}]" }),
        ),
    ] {
        send(
            &client,
            &t,
            "PUT",
            &format!("/api/translations/{kind}/{rid}/de"),
            json!({ "fields": fields }),
            200,
        )
        .await;
    }
    send(
        &client,
        &t,
        "POST",
        "/api/markets",
        json!({ "name": "Switzerland", "countries": ["CH"], "currency": "CHF",
                "base_currency": "EUR", "exchange_rate_ppm": 950_000, "rounding": "charm" }),
        201,
    )
    .await;

    // The shop's profile says what it publishes.
    let shop = public(&client, "acme", "/api/storefront/shop?country=ch", 200).await;
    assert_eq!(shop["default_language"], "en");
    assert_eq!(shop["languages"], json!(["de"]));
    assert_eq!(shop["language"], "en");
    assert_eq!(shop["prices_in"], "CHF");
    let shop = public(&client, "acme", "/api/storefront/shop?lang=fr", 200).await;
    assert_eq!(shop["language"], "en", "unpublished languages fall back");

    // Products, in the default language and shop prices, then in German
    // and Swiss francs.
    let list = public(&client, "acme", "/api/storefront/products", 200).await;
    assert_eq!(list["items"][0]["title"], "Trail Shoe");
    assert_eq!(list["items"][0]["price_minor"], 2000);
    let list = public(
        &client,
        "acme",
        "/api/storefront/products?lang=de&country=CH",
        200,
    )
    .await;
    assert_eq!(list["items"][0]["title"], "Trailschuh");
    assert_eq!(list["items"][0]["price_minor"], 1999);
    assert_eq!(list["items"][0]["currency"], "CHF");
    let one = public(
        &client,
        "acme",
        "/api/storefront/products/trail-shoe?lang=de&country=CH",
        200,
    )
    .await;
    assert_eq!(one["title"], "Trailschuh");
    assert_eq!(one["currency"], "CHF");
    // A /de/ prefix works as well as ?lang=.
    let one = public(
        &client,
        "acme",
        "/de/api/storefront/products/trail-shoe",
        200,
    )
    .await;
    assert_eq!(one["title"], "Trailschuh");
    assert_eq!(one["currency"], "EUR");

    // Collections, pages and menus.
    let cs = public(&client, "acme", "/api/storefront/collections?lang=de", 200).await;
    assert_eq!(cs["total"], 1);
    assert_eq!(cs["items"][0]["title"], "Schuhe");
    let handle = cs["items"][0]["handle"].as_str().unwrap().to_owned();
    let one = public(
        &client,
        "acme",
        &format!("/api/storefront/collections/{handle}"),
        200,
    )
    .await;
    assert_eq!(one["title"], "Shoes");
    let handle = page["handle"].as_str().unwrap();
    let p = public(
        &client,
        "acme",
        &format!("/api/storefront/pages/{handle}?lang=de"),
        200,
    )
    .await;
    assert_eq!(p["title"], "Über");
    let m = public(&client, "acme", "/api/storefront/menus/main?lang=de", 200).await;
    assert_eq!(m["items"][0]["title"], "Über uns");
    assert_eq!(m["items"][0]["url"], "/de/pages/about");
    let m = public(&client, "acme", "/api/storefront/menus/main", 200).await;
    assert_eq!(m["items"][0]["url"], "/pages/about");

    // Another shop sees none of it.
    let other = public(&client, "globex", "/api/storefront/products", 200).await;
    assert_eq!(other["total"], 0);
    let other = public(&client, "globex", "/api/storefront/collections", 200).await;
    assert_eq!(other["total"], 0);
    public(
        &client,
        "globex",
        "/api/storefront/products/trail-shoe",
        404,
    )
    .await;
    public(&client, "globex", "/api/storefront/menus/main", 404).await;
}

#[tokio::test]
async fn browsers_may_call_the_storefront_api_from_any_origin() {
    let (client, _t) = common::with_tenants(&["acme"]).await;
    let res = client
        .request(Method::OPTIONS, "/api/storefront/products")
        .header("origin", "https://shop.example.com")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization, x-soat-tenant",
        )
        .send()
        .await;
    res.assert_status(204);
    assert_eq!(res.header("access-control-allow-origin"), Some("*"));
    let allowed = res.header("access-control-allow-headers").unwrap();
    for h in [
        "authorization",
        "x-soat-tenant",
        "x-soat-cart-token",
        "x-soat-order-token",
    ] {
        assert!(allowed.contains(h), "{allowed}");
    }
    assert!(res.header("access-control-allow-credentials").is_none());

    // The admin API stays same-origin.
    let res = client
        .request(Method::OPTIONS, "/api/products")
        .header("origin", "https://shop.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await;
    assert!(res.header("access-control-allow-origin").is_none());
    let res = client
        .get("/api/shop")
        .header("x-soat-tenant", "acme")
        .header("origin", "https://shop.example.com")
        .send()
        .await;
    assert!(res.header("access-control-allow-origin").is_none());
}
