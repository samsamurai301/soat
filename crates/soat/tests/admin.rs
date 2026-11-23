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
    assert!(res.text().contains("<strong>0</strong> products"));

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
