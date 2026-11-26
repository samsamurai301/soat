//! I1: storefront analytics. Visits are counted without cookies, bots
//! and API calls left out; the report shows visitors, pages, sources,
//! devices and the way to a paid order.
#![cfg(feature = "analytics")]

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Opens storefront `path` of `shop` as a browser.
async fn visit(c: &TestClient, shop: &str, path: &str, who: (&str, &str), referer: &str) -> u16 {
    let req = c
        .get(path)
        .header("x-soat-tenant", shop)
        .header("x-forwarded-for", who.0)
        .header("user-agent", who.1);
    let req = if referer.is_empty() {
        req
    } else {
        req.header("referer", referer)
    };
    req.send().await.status().as_u16()
}

const DESK: (&str, &str) = (
    "203.0.113.7",
    "Mozilla/5.0 (X11; Linux x86_64) Gecko/20100101 Firefox/130.0",
);
const PHONE: (&str, &str) = (
    "198.51.100.9",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0) Mobile/15E148 Safari/604.1",
);
const BOT: (&str, &str) = ("192.0.2.1", "Mozilla/5.0 (compatible; Googlebot/2.1)");

#[tokio::test]
async fn visits_are_counted_without_cookies() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let mug = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "MUG", "title": "Mug", "price_minor": 1200, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Two people, one bot; API calls and missing pages do not count.
    assert_eq!(visit(c, "acme", "/", DESK, "").await, 200);
    assert_eq!(visit(c, "acme", "/products/mug", DESK, "").await, 200);
    assert_eq!(
        visit(c, "acme", "/products/mug?utm_source=Newsletter", PHONE, "").await,
        200
    );
    assert_eq!(
        visit(c, "acme", "/", PHONE, "https://www.google.com/search?q=mug").await,
        200
    );
    assert_eq!(visit(c, "acme", "/", BOT, "").await, 200);
    assert_eq!(visit(c, "acme", "/products/nope", DESK, "").await, 404);
    assert_eq!(
        visit(c, "acme", "/api/storefront/products", DESK, "").await,
        200
    );
    assert_eq!(visit(c, "other", "/", DESK, "").await, 200);
    // Nothing goes in a cookie.
    let res = c
        .get("/")
        .header("x-soat-tenant", "acme")
        .header("x-forwarded-for", DESK.0)
        .header("user-agent", DESK.1)
        .send()
        .await;
    assert!(res.header("set-cookie").is_none());

    // A cart with a line and two paid orders by one buyer.
    let cart = call(
        c,
        "acme",
        "POST",
        "/api/storefront/carts",
        None,
        json!({ "currency": "EUR" }),
        201,
    )
    .await;
    let id = cart["cart"]["id"].as_str().unwrap();
    c.post(&format!("/api/storefront/carts/{id}/lines"))
        .header("x-soat-tenant", "acme")
        .header("x-soat-cart-token", cart["token"].as_str().unwrap())
        .json(&json!({ "item_id": mug, "quantity": 1 }))
        .send()
        .await
        .assert_status(200);
    for _ in 0..2 {
        let d = call(
            c,
            "acme",
            "POST",
            "/api/draft-orders",
            a,
            json!({ "currency": "EUR", "email": "ada@example.com",
                    "billing_address": { "line1": "Hauptstr. 1", "postal_code": "10115",
                                         "city": "Berlin", "country": "DE" },
                    "lines": [{ "item_id": mug, "quantity": 1 }] }),
            201,
        )
        .await;
        call(
            c,
            "acme",
            "POST",
            &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
            a,
            json!({ "paid": true }),
            201,
        )
        .await;
    }

    let r = call(
        c,
        "acme",
        "GET",
        "/api/analytics?days=7",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        (r["visitors"].clone(), r["views"].clone()),
        (json!(2), json!(5)),
        "{r}"
    );
    assert_eq!(r["days"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["days"][0]["visitors"], 2);
    assert_eq!(
        r["pages"][0],
        json!({ "path": "/", "views": 3, "visitors": 2 })
    );
    assert_eq!(r["pages"][1]["path"], "/products/mug");
    let names = |k: &str| -> Vec<(String, i64)> {
        r[k].as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap().to_owned(),
                    s["visitors"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(
        names("sources"),
        [
            ("direct".into(), 1),
            ("google.com".into(), 1),
            ("newsletter".into(), 1)
        ]
    );
    assert_eq!(
        names("devices"),
        [("desktop".into(), 1), ("mobile".into(), 1)]
    );
    assert_eq!(
        r["funnel"],
        json!({ "visitors": 2, "carts": 1, "orders": 2, "paid": 2, "conversion_bps": 10000 })
    );
    assert_eq!(
        r["buyers"],
        json!({ "first_orders": 1, "repeat_orders": 1 })
    );
    call(
        c,
        "acme",
        "GET",
        "/api/analytics?days=0",
        a,
        Value::Null,
        422,
    )
    .await;

    // Each shop sees only its own visits.
    let theirs = call(c, "other", "GET", "/api/analytics", a, Value::Null, 200).await;
    assert_eq!(
        (
            theirs["visitors"].clone(),
            theirs["funnel"]["orders"].clone()
        ),
        (json!(1), json!(0))
    );

    // The admin's analytics page.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/analytics?days=7")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("From visit to payment") && html.contains("google.com"),
        "{html}"
    );
    assert!(html.contains("100.0%"), "{html}");
}
