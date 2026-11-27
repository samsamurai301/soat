//! J2: cookie consent. SOAT loads nothing from a third party on its own;
//! with the banner on, tracking loads only for what a visitor agreed to,
//! and each choice is recorded without who made it.
#![cfg(feature = "privacy")]

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
        "PUT" => client.put(path),
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

async fn home(c: &TestClient, consent: Option<&str>) -> String {
    let req = c.get("/").header("x-soat-tenant", "acme");
    let req = match consent {
        Some(v) => req.header("cookie", &format!("soat_consent={v}")),
        None => req,
    };
    let res = req.send().await;
    res.assert_status(200);
    res.text()
}

/// Chooses in the banner; the consent cookie's value.
async fn choose(c: &TestClient, form: Value) -> (String, String) {
    let res = c
        .post("/consent")
        .header("x-soat-tenant", "acme")
        .form(&form)
        .send()
        .await;
    res.assert_status(303);
    let at = res.header("location").unwrap().to_owned();
    (at, res.cookie("soat_consent").unwrap().value().to_owned())
}

#[tokio::test]
async fn the_banner_asks_before_tracking_loads() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());

    // Without a banner nothing is asked, and nothing comes from elsewhere.
    let html = home(c, None).await;
    assert!(!html.contains("class=\"consent\""), "{html}");
    assert!(html.contains("/assets/htmx-1.9.12.min.js"));
    for third in ["unpkg.com", "googletagmanager", "fbevents"] {
        assert!(!html.contains(third), "{third} in {html}");
    }
    let res = c.get("/assets/htmx-1.9.12.min.js").send().await;
    res.assert_status(200);
    assert!(res.header("content-type").unwrap().contains("javascript"));
    assert!(res.header("cache-control").unwrap().contains("immutable"));
    assert!(res.text().contains("htmx"));

    // Tracking codes are checked; the banner is turned on.
    call(
        c,
        "acme",
        "PUT",
        "/api/consent",
        a,
        json!({ "ga4_id": "UA-123" }),
        422,
    )
    .await;
    call(
        c,
        "acme",
        "PUT",
        "/api/consent",
        a,
        json!({ "meta_pixel_id": "12ab" }),
        422,
    )
    .await;
    let s = call(
        c,
        "acme",
        "PUT",
        "/api/consent",
        a,
        json!({ "enabled": true, "ga4_id": "G-TEST123", "meta_pixel_id": "1234567890",
                "privacy_url": "/pages/privacy" }),
        200,
    )
    .await;
    assert_eq!(s["version"], 1);

    // Asked; nothing loads until a choice.
    let html = home(c, None).await;
    assert!(html.contains("class=\"consent\""), "{html}");
    assert!(html.contains("Reject all") && html.contains("Accept all"));
    assert!(html.contains("/pages/privacy"));
    assert!(!html.contains("googletagmanager") && !html.contains("fbevents"));

    // Rejecting: no tracking, no banner again.
    let (back, none) = choose(c, json!({ "choice": "none", "back": "/search" })).await;
    assert_eq!(back, "/search");
    assert_eq!(none, "1.");
    let html = home(c, Some(&none)).await;
    assert!(!html.contains("class=\"consent\""));
    assert!(!html.contains("googletagmanager") && !html.contains("fbevents"));
    assert!(html.contains("/consent"), "the footer links the settings");

    // Analytics only.
    let (_, only) = choose(
        c,
        json!({ "choice": "chosen", "analytics": "1", "back": "/" }),
    )
    .await;
    assert_eq!(only, "1.a");
    let html = home(c, Some(&only)).await;
    assert!(
        html.contains("googletagmanager.com/gtag/js?id=G-TEST123"),
        "{html}"
    );
    assert!(html.contains("ad_storage:'denied'"));
    assert!(!html.contains("fbevents"));

    // Everything; a foreign "back" goes home instead.
    let (back, all) = choose(c, json!({ "choice": "all", "back": "//evil.example/x" })).await;
    assert_eq!(back, "/");
    assert_eq!(all, "1.amx");
    let html = home(c, Some(&all)).await;
    assert!(html.contains("G-TEST123") && html.contains("fbq('init','1234567890')"));
    assert!(html.contains("ad_storage:'granted'"));

    // A new version asks everyone again.
    let s = call(
        c,
        "acme",
        "PUT",
        "/api/consent",
        a,
        json!({ "ask_again": true }),
        200,
    )
    .await;
    assert_eq!(s["version"], 2);
    let html = home(c, Some(&all)).await;
    assert!(html.contains("class=\"consent\""));
    assert!(!html.contains("googletagmanager"));

    // The settings page; a headless storefront records its own choice.
    let page = c
        .get("/consent")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .text();
    assert!(
        page.contains("Save my choice") && page.contains("noindex"),
        "{page}"
    );
    let h = call(
        c,
        "acme",
        "GET",
        "/api/storefront/consent",
        None,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(h["version"], 2);
    let given = call(
        c,
        "acme",
        "POST",
        "/api/storefront/consent",
        None,
        json!({ "analytics": true, "marketing": false, "apps": false }),
        201,
    )
    .await;
    assert_eq!(given["cookie"], "2.a");

    // Proof of the choices, without who made them.
    let st = call(c, "acme", "GET", "/api/consent/stats", a, Value::Null, 200).await;
    assert_eq!(st["choices"], 4);
    assert_eq!(st["accepted_all"], 1);
    assert_eq!(st["rejected_all"], 1);
    assert_eq!(st["analytics"], 3);
    let e = call(
        c,
        "acme",
        "GET",
        "/api/events?kind=consent.updated",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(e["items"].as_array().unwrap().len(), 2);

    // Another shop keeps its own.
    let theirs = call(c, "other", "GET", "/api/consent", a, Value::Null, 200).await;
    assert_eq!(theirs["enabled"], false);
    let st = call(c, "other", "GET", "/api/consent/stats", a, Value::Null, 200).await;
    assert_eq!(st["choices"], 0);

    // The admin page.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/privacy")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Cookie banner") && html.contains("version 2") && html.contains("G-TEST123"),
        "{html}"
    );
    assert!(!html.contains("unpkg.com"));
    let html = c
        .post("/admin/acme/privacy/consent")
        .header("cookie", &cookie)
        .form(&json!({ "enabled": "on", "ga4_id": "nope" }))
        .send()
        .await
        .text();
    assert!(html.contains("looks like G-XXXXXXXXXX"), "{html}");
    let res = c.get("/admin/htmx-1.9.12.min.js").send().await;
    res.assert_status(200);
}
