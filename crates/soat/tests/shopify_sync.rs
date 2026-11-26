//! G6: a Shopify store synced by its Admin API until cutover.
#![cfg(feature = "shopify-sync")]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use soat_testing::{fake_api, Log, Received};
use zapline::db::Database;
use zapline::test::TestClient;

fn products(renamed: bool) -> Value {
    json!({ "products": [
        { "id": 101, "handle": "trail-shoe",
          "title": if renamed { "Trail Shoe Pro" } else { "Trail Shoe" },
          "body_html": "<p>Light.</p>", "status": "active", "tags": "shoes",
          "options": [{ "name": "Size" }],
          "variants": [
            { "sku": "TS-42", "price": "120.00", "option1": "42", "grams": 300,
              "inventory_management": "shopify", "inventory_quantity": 3 },
            { "sku": "TS-43", "price": "120.00", "option1": "43", "grams": 300,
              "inventory_management": "shopify", "inventory_quantity": 1 }
          ] },
        { "id": 102, "handle": "sock", "title": "Sock", "status": "draft",
          "options": [{ "name": "Title" }],
          "variants": [{ "sku": "SOCK", "price": "9.00", "option1": "Default Title" }] }
    ] })
}

async fn setup() -> (TestClient, String, Log, Arc<AtomicBool>) {
    let renamed = Arc::new(AtomicBool::new(false));
    let flag = renamed.clone();
    let (base, log) = fake_api(move |r: &Received| {
        if r.header("x-shopify-access-token") != Some("shpat_good") {
            return (
                401,
                json!({ "errors": "[API] Invalid API key or access token" }),
            );
        }
        let path = r.path.split('?').next().unwrap_or_default();
        let later = r.path.contains("since_id=") && !r.path.contains("since_id=0");
        match path {
            "/admin/api/2025-01/shop.json" => (200, json!({ "shop": { "name": "Acme" } })),
            "/admin/api/2025-01/products.json" if later => (200, json!({ "products": [] })),
            "/admin/api/2025-01/products.json" => (200, products(flag.load(Ordering::SeqCst))),
            "/admin/api/2025-01/customers.json" => (
                200,
                json!({ "customers": [{ "id": 7, "email": "ada@example.com",
                    "first_name": "Ada", "last_name": "Lovelace",
                    "email_marketing_consent": { "state": "subscribed" } }] }),
            ),
            _ => (404, json!({ "errors": "Not Found" })),
        }
    })
    .await;
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.shopify_api_base = Some(base);
    let client = soat::app(Database::postgres(url), settings).test();
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, &["acme", "globex"]).await;
    (client, token, log, renamed)
}

async fn send(
    client: &TestClient,
    token: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    if status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn sign_in(client: &TestClient) -> String {
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    format!("soat_admin={}", res.cookie("soat_admin").unwrap().value())
}

async fn sync(client: &TestClient, cookie: &str, cutover: bool) -> String {
    let body = if cutover {
        json!({ "cutover": "1" })
    } else {
        json!({})
    };
    let res = client
        .post("/admin/acme/transfer/shopify/sync")
        .header("cookie", cookie)
        .form(&body)
        .send()
        .await;
    let html = res.text();
    assert_eq!(res.status(), 200, "{html}");
    html
}

#[tokio::test]
async fn shops_sync_from_shopify_until_cutover() {
    let (client, t, log, renamed) = setup().await;
    send(
        &client,
        &t,
        "acme",
        "PATCH",
        "/api/shop",
        json!({ "currency": "EUR" }),
        200,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/imports/shopify",
        Value::Null,
        404,
    )
    .await;

    // Only *.myshopify.com, and a token Shopify accepts.
    let e = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/imports/shopify",
        json!({ "shop_domain": "evil.example.com", "access_token": "shpat_good" }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_shop_domain");
    let e = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/imports/shopify",
        json!({ "shop_domain": "acme.myshopify.com", "access_token": "shpat_bad" }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_credentials");
    let c = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/imports/shopify",
        json!({ "shop_domain": "https://Acme.myshopify.com/", "access_token": "shpat_good" }),
        200,
    )
    .await;
    assert_eq!(c["shop_domain"], "acme.myshopify.com");
    assert!(c.get("access_token").is_none() && c.get("token").is_none());

    // A pull is SOAT's import file.
    let pulled = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/imports/shopify/pull",
        json!({ "what": "products" }),
        200,
    )
    .await;
    assert_eq!(pulled["count"], 2);
    assert!(pulled["csv"].as_str().unwrap().contains("trail-shoe"));
    assert_eq!(pulled["more"], false);

    // The admin's sync imports products and customers, and records them.
    let me = sign_in(&client).await;
    let page = client
        .get("/admin/acme/transfer")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(page.contains("acme.myshopify.com"), "{page}");
    let html = sync(&client, &me, false).await;
    assert!(html.contains("3 created, 0 updated"), "{html}");
    let list = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/products?sort=title",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 2);
    assert_eq!(list["items"][1]["title"], "Trail Shoe");
    let customers = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/customers",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(customers["items"][0]["accepts_marketing"], true);
    let state = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/imports/shopify",
        Value::Null,
        200,
    )
    .await;
    assert!(state["products_synced_at"].is_string());
    assert!(state["customers_synced_at"].is_string());

    // The next sync asks only for what changed since.
    renamed.store(true, Ordering::SeqCst);
    let html = sync(&client, &me, false).await;
    assert!(html.contains("0 created, 3 updated"), "{html}");
    let list = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/products?sort=title",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["items"][1]["title"], "Trail Shoe Pro");
    let asked: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.path.starts_with("/admin/api/2025-01/products.json"))
        .map(|r| r.path.clone())
        .collect();
    assert!(!asked[0].contains("updated_at_min"));
    assert!(
        asked.last().unwrap().contains("updated_at_min="),
        "{asked:?}"
    );

    // Another shop sees nothing of it.
    send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/imports/shopify",
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        &t,
        "globex",
        "POST",
        "/api/imports/shopify/pull",
        json!({ "what": "products" }),
        404,
    )
    .await;

    // Cutover is the last sync; pulls stop after it.
    let html = sync(&client, &me, true).await;
    assert!(html.contains("Cut over on"), "{html}");
    let e = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/imports/shopify/pull",
        json!({ "what": "customers" }),
        409,
    )
    .await;
    assert_eq!(e["code"], "cut_over");

    // The token never reaches the event log.
    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?limit=100",
        Value::Null,
        200,
    )
    .await;
    let text = events.to_string();
    assert!(text.contains("shopify_import.connected"));
    assert!(text.contains("shopify_import.cut_over"));
    assert!(!text.contains("shpat_good"));

    send(
        &client,
        &t,
        "acme",
        "DELETE",
        "/api/imports/shopify",
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/imports/shopify",
        Value::Null,
        404,
    )
    .await;
}
