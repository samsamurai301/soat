//! K3: Content Security Policy. Every page names a fresh nonce, every
//! script and style its templates wrote carries it, and nothing else does.

mod common;

use serde_json::json;
use zapline::test::TestClient;

/// The page at `path`: its policy's nonce and its HTML. Checks that every
/// `<script>` and `<style>` carries that nonce.
async fn page(c: &TestClient, path: &str, headers: &[(&str, &str)]) -> (String, String) {
    let req = headers
        .iter()
        .fold(c.get(path), |r, (k, v)| r.header(*k, *v));
    let res = req.send().await;
    let policy = res
        .header("content-security-policy")
        .unwrap_or_else(|| panic!("no policy on {path}"));
    let nonce = policy
        .split("'nonce-")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .unwrap_or_else(|| panic!("no nonce in {policy}"))
        .to_owned();
    assert!(policy.contains("object-src 'none'"), "{policy}");
    let html = res.text();
    assert!(
        !html.contains("soat-csp-"),
        "the placeholder leaked on {path}"
    );
    let lower = html.to_ascii_lowercase();
    for tag in ["<script", "<style"] {
        for (i, _) in lower.match_indices(tag) {
            let rest = &html[i..];
            if !rest[tag.len()..].starts_with([' ', '>']) {
                continue;
            }
            let end = rest.find('>').unwrap();
            assert!(
                rest[..end].contains(&format!("nonce=\"{nonce}\"")),
                "{path}: {}",
                &rest[..end]
            );
        }
    }
    (nonce, html)
}

#[tokio::test]
async fn pages_carry_a_fresh_nonce_on_their_own_scripts() {
    let (c, token) = common::with_tenants(&["acme"]).await;
    c.put("/api/consent")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&json!({ "enabled": true, "ga4_id": "G-TEST123", "meta_pixel_id": "1234567890" }))
        .send()
        .await
        .assert_status(200);
    let shop = [("x-soat-tenant", "acme")];
    let (first, _) = page(&c, "/", &shop).await;
    let (second, _) = page(&c, "/", &shop).await;
    assert_ne!(first, second, "a nonce is used once");
    assert_eq!(first.len(), 32);

    // Tracking after consent: its scripts run, by nonce.
    let tracked = [("x-soat-tenant", "acme"), ("cookie", "soat_consent=1.am")];
    let (_, html) = page(&c, "/", &tracked).await;
    assert!(html.contains("googletagmanager.com/gtag/js"), "{html}");
    assert!(html.contains("fbq('init'"), "{html}");

    for path in ["/account/login", "/search?q=x", "/no-such-page", "/cart"] {
        page(&c, path, &shop).await;
    }

    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let staff = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let (_, html) = page(&c, "/admin/acme", &[("cookie", &staff)]).await;
    assert!(
        html.contains("palette"),
        "the admin's own script is on the page"
    );
    page(&c, "/admin/login", &[]).await;
}

#[tokio::test]
async fn content_cannot_bring_a_script() {
    let (c, token) = common::with_tenants(&["acme"]).await;
    c.post("/api/pages")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&json!({ "title": "Hi", "published": true, "blocks": [
            { "type": "text", "body": "<script>alert(1)</script>" }
        ] }))
        .send()
        .await
        .assert_status(201);
    let (_, html) = page(&c, "/pages/hi", &[("x-soat-tenant", "acme")]).await;
    assert!(html.contains("&lt;script&gt;alert(1)"), "{html}");
}

#[tokio::test]
async fn the_api_docs_keep_their_own_assets() {
    let c = common::client().await;
    let res = c.get("/docs").send().await;
    assert!(res.header("content-security-policy").is_none());
}
