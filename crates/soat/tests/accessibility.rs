//! J5: accessibility. Every page the admin and the storefront show, as a
//! visitor, a signed-in customer and staff, is crawled from its start page
//! and checked against the WCAG criteria its markup alone can break
//! (`soat_testing::a11y`).

mod common;

use std::collections::{BTreeMap, HashSet, VecDeque};

use serde_json::json;
use zapline::test::TestClient;

/// Paths not followed: downloads, sign-outs, feeds and assets.
fn skipped(path: &str) -> bool {
    const SKIP: [&str; 14] = [
        "logout",
        "/assets/",
        "/media/",
        "/files/",
        "/api/",
        "/e-invoice",
        "/document",
        "personal-data",
        "/export",
        "/download",
        "/feeds/",
        "sitemap",
        ".xml",
        "/htmx-",
    ];
    SKIP.iter().any(|s| path.contains(s))
        || [
            ".json", ".js", ".css", ".txt", ".csv", ".png", ".jpg", ".svg", ".webp",
        ]
        .iter()
        .any(|e| path.split('?').next().unwrap_or_default().ends_with(e))
}

/// Same-site links of a page.
fn links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in html.split("href=\"").skip(1) {
        let Some(end) = part.find('"') else { continue };
        let href = part[..end].replace("&amp;", "&");
        let href = href.split('#').next().unwrap_or_default().to_owned();
        if href.starts_with('/') && !href.starts_with("//") {
            out.push(href);
        }
    }
    out
}

/// Crawls from `start` through links under `prefix`; checks every HTML page.
async fn crawl(
    c: &TestClient,
    headers: &[(&str, &str)],
    start: &[&str],
    prefix: &str,
    exclude: &str,
    found: &mut BTreeMap<String, Vec<String>>,
) -> usize {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = start.iter().map(|s| (*s).to_owned()).collect();
    let mut pages = 0;
    while let Some(path) = queue.pop_front() {
        if !seen.insert(path.clone()) || seen.len() > 400 {
            continue;
        }
        let req = headers
            .iter()
            .fold(c.get(&path), |r, (k, v)| r.header(*k, *v));
        let res = req.send().await;
        let html_page = res
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/html"));
        if !matches!(res.status().as_u16(), 200 | 404) || !html_page {
            continue;
        }
        let html = res.text();
        pages += 1;
        for issue in soat_testing::a11y::check(&html) {
            found
                .entry(issue.to_string())
                .or_default()
                .push(path.clone());
        }
        for link in links(&html) {
            if link.starts_with(prefix)
                && (exclude.is_empty() || !link.starts_with(exclude))
                && !skipped(&link)
                && !seen.contains(&link)
            {
                queue.push_back(link);
            }
        }
    }
    pages
}

#[tokio::test]
async fn every_page_passes_the_accessibility_checks() {
    let (c, token) = common::with_tenants(&["acme"]).await;
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let staff = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    for (step, form) in [
        (
            "basics",
            json!({ "name": "Acme", "country": "DE", "currency": "EUR" }),
        ),
        ("sample", json!({ "template": "coffee" })),
    ] {
        c.post(&format!("/admin/acme/welcome/{step}"))
            .header("cookie", &staff)
            .form(&form)
            .send()
            .await
            .assert_status(303);
    }
    // Content of every kind, the cookie banner on, a cart with something in it.
    for (path, body) in [
        ("/api/collections", json!({ "title": "Coffee" })),
        (
            "/api/pages",
            json!({ "title": "About Us", "published": true, "blocks": [
                { "type": "heading", "text": "About us", "level": 1 },
                { "type": "text", "body": "We roast *coffee*." },
                { "type": "button", "label": "Shop now", "url": "/search" }
            ] }),
        ),
        (
            "/api/blog/posts",
            json!({ "title": "Hello world", "excerpt": "We opened.", "body": "We *opened*.", "tags": ["News"], "published": true }),
        ),
    ] {
        c.post(path)
            .header("x-soat-tenant", "acme")
            .bearer(&token)
            .json(&body)
            .send()
            .await
            .assert_status(201);
    }
    c.put("/api/consent")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .json(&json!({ "enabled": true, "privacy_url": "/pages/about-us" }))
        .send()
        .await
        .assert_status(200);
    let products = c
        .get("/api/products?limit=50")
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .json::<serde_json::Value>();
    let mut cart = None;
    for p in products["items"].as_array().unwrap() {
        let res = c
            .post("/cart/add")
            .header("x-soat-tenant", "acme")
            .form(&json!({ "product_id": p["id"], "quantity": "1" }))
            .send()
            .await;
        if res.status().as_u16() == 303 {
            cart = res
                .header("set-cookie")
                .and_then(|h| h.split(';').next())
                .map(str::to_owned);
            break;
        }
    }
    let cart = cart.expect("a product without variants to put in the cart");

    let customer = c
        .post("/api/storefront/account/register")
        .header("x-soat-tenant", "acme")
        .json(&json!({ "email": "ada@example.com", "password": "a long password" }))
        .send()
        .await
        .assert_status(201)
        .json::<serde_json::Value>()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let customer = format!("soat_customer={customer}");

    let mut found = BTreeMap::new();
    let admin = crawl(
        &c,
        &[("cookie", &staff)],
        &["/admin/acme", "/admin/acme/settings", "/admin/acme/welcome"],
        "/admin/acme",
        "",
        &mut found,
    )
    .await;
    let visitor = crawl(
        &c,
        &[("x-soat-tenant", "acme")],
        &[
            "/",
            "/search?q=coffee",
            "/collections/coffee",
            "/pages/about-us",
            "/blog",
            "/account/login",
            "/account/register",
            "/consent",
            "/no-such-page",
        ],
        "/",
        "/admin",
        &mut found,
    )
    .await;
    let signed_in = crawl(
        &c,
        &[("x-soat-tenant", "acme"), ("cookie", &customer)],
        &["/account"],
        "/account",
        "",
        &mut found,
    )
    .await;
    let shopper = crawl(
        &c,
        &[("x-soat-tenant", "acme"), ("cookie", &cart)],
        &["/cart", "/checkout"],
        "/checkout",
        "",
        &mut found,
    )
    .await;
    assert!(shopper >= 2, "only {shopper} cart and checkout pages");
    let login = crawl(
        &c,
        &[],
        &["/admin/login", "/admin/setup"],
        "/admin/login",
        "",
        &mut found,
    )
    .await;
    assert!(admin > 40, "only {admin} admin pages");
    assert!(visitor > 10, "only {visitor} storefront pages");
    assert!(signed_in > 1, "only {signed_in} account pages");
    assert!(login >= 1);
    let report: Vec<String> = found
        .iter()
        .map(|(issue, pages)| {
            let mut pages = pages.clone();
            pages.dedup();
            let more = pages.len().saturating_sub(3);
            pages.truncate(3);
            format!(
                "{issue}\n    on {}{}",
                pages.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        })
        .collect();
    assert!(
        report.is_empty(),
        "{} accessibility issues on {admin} admin, {visitor} storefront and {signed_in} account pages:\n{}",
        report.len(),
        report.join("\n")
    );
}
