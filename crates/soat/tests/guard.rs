//! K1: requests are guarded before any module sees them: other sites
//! cannot post the admin's or the storefront's forms, and guessing
//! passwords and codes is slowed down.

mod common;

use serde_json::json;

#[tokio::test]
async fn other_sites_cannot_post_forms() {
    let (c, _) = common::with_tenants(&["acme"]).await;
    let login = json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD });
    for (header, value) in [
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
        ("origin", "https://evil.example"),
    ] {
        let res = c
            .post("/admin/login")
            .header(header, value)
            .form(&login)
            .send()
            .await;
        assert_eq!(res.status().as_u16(), 403, "{header}: {value}");
        let res = c
            .post("/cart/add")
            .header("x-soat-tenant", "acme")
            .header(header, value)
            .form(&json!({ "product_id": "prd_x", "quantity": "1" }))
            .send()
            .await;
        assert_eq!(res.status().as_u16(), 403, "storefront, {header}: {value}");
    }
    // The browser's own pages, and clients that are not browsers, pass.
    let res = c
        .post("/admin/login")
        .header("sec-fetch-site", "same-origin")
        .form(&login)
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 303);
    c.post("/admin/login")
        .form(&login)
        .send()
        .await
        .assert_status(303);
    // The API answers other origins by design: it takes no cookies.
    let res = c
        .post("/api/storefront/account/login")
        .header("x-soat-tenant", "acme")
        .header("sec-fetch-site", "cross-site")
        .header("origin", "https://headless.example")
        .json(&json!({ "email": "nobody@example.com", "password": "wrong password" }))
        .send()
        .await;
    assert_ne!(res.status().as_u16(), 403);
}

#[tokio::test]
async fn guessing_is_limited_per_endpoint() {
    let (c, _) = common::with_tenants(&["acme"]).await;
    let attempt = |path: &'static str| {
        let c = &c;
        async move {
            c.post(path)
                .header("x-soat-tenant", "acme")
                .json(&json!({ "email": "nobody@example.com", "password": "wrong password", "code": "XXXX-XXXX-XXXX-XXXX" }))
                .send()
                .await
                .status()
                .as_u16()
        }
    };
    for _ in 0..soat::guard::GUESSES_PER_MINUTE {
        assert_ne!(attempt("/api/storefront/account/login").await, 429);
    }
    // The allowance refills over the minute (a password check takes a
    // moment), so the limit comes within a few more tries.
    let mut limited = false;
    for _ in 0..10 {
        if attempt("/api/storefront/account/login").await == 429 {
            limited = true;
            break;
        }
    }
    assert!(limited, "never limited");
    // Another endpoint has its own allowance; reading is never limited.
    assert_ne!(attempt("/api/storefront/gift-cards/balance").await, 429);
    c.get("/api/storefront/products")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(200);
}
