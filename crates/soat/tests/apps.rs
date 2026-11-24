//! F4: apps registered by developers, installed by shops with consent,
//! acting with exactly the permissions granted.
#![cfg(feature = "apps")]

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    bearer: Option<&str>,
    shop: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let mut req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    };
    if let Some(s) = shop {
        req = req.header("x-soat-tenant", s);
    }
    if let Some(b) = bearer {
        req = req.bearer(b);
    }
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn query(url: &str, name: &str) -> String {
    let q = url.split_once('?').unwrap().1;
    q.split('&')
        .find_map(|p| p.strip_prefix(&format!("{name}=")))
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn shops_install_apps_with_consent() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;

    // A developer registers an app.
    for bad in [
        json!({ "name": "X", "redirect_uris": ["http://app.example.com/cb"], "permissions": ["products:read"] }),
        json!({ "name": "X", "redirect_uris": ["https://app.example.com/cb"], "permissions": ["rockets:launch"] }),
        json!({ "name": "X", "redirect_uris": [], "permissions": ["products:read"] }),
    ] {
        call(&client, Some(&t), None, "POST", "/api/apps", bad, 422).await;
    }
    call(&client, None, None, "POST", "/api/apps", json!({}), 401).await;
    let reg = call(
        &client,
        Some(&t),
        None,
        "POST",
        "/api/apps",
        json!({ "name": "Reviews", "redirect_uris": ["https://reviews.example.com/cb"],
                "permissions": ["products:read", "orders:read"] }),
        201,
    )
    .await;
    let client_id = reg["app"]["id"].as_str().unwrap().to_owned();
    let secret = reg["client_secret"].as_str().unwrap().to_owned();
    assert!(client_id.starts_with("app_") && secret.starts_with("soat_cs_"));
    let mine = call(
        &client,
        Some(&t),
        None,
        "GET",
        "/api/apps",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert!(mine[0].get("client_secret").is_none());

    // The consent screen says what it asks for.
    let consent = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        &format!("/api/app-consent/{client_id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(consent["name"], "Reviews");
    assert_eq!(consent["permissions"][0]["held"], true);
    assert!(consent["permissions"][0]["description"].is_string());

    // Only to a registered address.
    let e = call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/app-installations",
        json!({ "client_id": client_id, "redirect_uri": "https://evil.example.com/cb" }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_redirect");
    let installed = call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/app-installations",
        json!({ "client_id": client_id, "redirect_uri": "https://reviews.example.com/cb",
                "state": "xyz" }),
        201,
    )
    .await;
    let to = installed["redirect_to"].as_str().unwrap();
    assert!(
        to.starts_with("https://reviews.example.com/cb?code="),
        "{to}"
    );
    assert_eq!(query(to, "shop"), "acme");
    assert_eq!(query(to, "state"), "xyz");
    let code = query(to, "code");
    assert_eq!(installed["installation"]["connected"], false);

    // The app trades the code for a key, once.
    let trade = |code: String, secret: String| {
        json!({ "client_id": client_id, "client_secret": secret, "code": code,
                "shop": "acme", "redirect_uri": "https://reviews.example.com/cb" })
    };
    let e = call(
        &client,
        None,
        None,
        "POST",
        "/api/apps/token",
        trade(code.clone(), "soat_cs_wrong".into()),
        400,
    )
    .await;
    assert_eq!(e["code"], "invalid_grant");
    let tok = call(
        &client,
        None,
        None,
        "POST",
        "/api/apps/token",
        trade(code.clone(), secret.clone()),
        200,
    )
    .await;
    let key = tok["access_token"].as_str().unwrap().to_owned();
    assert!(key.starts_with("soat_sk_acme_"));
    assert_eq!(tok["permissions"], json!(["orders:read", "products:read"]));
    call(
        &client,
        None,
        None,
        "POST",
        "/api/apps/token",
        trade(code, secret.clone()),
        400,
    )
    .await;

    // The key does what was granted, nothing more, in its own shop only.
    call(
        &client,
        Some(&key),
        None,
        "GET",
        "/api/products",
        Value::Null,
        200,
    )
    .await;
    call(
        &client,
        Some(&key),
        None,
        "POST",
        "/api/products",
        json!({ "sku": "X", "title": "X", "price_minor": 1, "currency": "EUR" }),
        403,
    )
    .await;
    call(
        &client,
        Some(&key),
        Some("globex"),
        "GET",
        "/api/products",
        Value::Null,
        404,
    )
    .await;

    // Another shop sees no install; the list shows it connected.
    let other = call(
        &client,
        Some(&t),
        Some("globex"),
        "GET",
        "/api/app-installations",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(other, json!([]));
    let list = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-installations",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list[0]["connected"], true);
    let id = list[0]["id"].as_str().unwrap().to_owned();
    call(
        &client,
        Some(&t),
        Some("globex"),
        "DELETE",
        &format!("/api/app-installations/{id}"),
        Value::Null,
        404,
    )
    .await;

    // Uninstalling stops the key at once, and says so.
    call(
        &client,
        Some(&t),
        Some("acme"),
        "DELETE",
        &format!("/api/app-installations/{id}"),
        Value::Null,
        204,
    )
    .await;
    call(
        &client,
        Some(&key),
        None,
        "GET",
        "/api/products",
        Value::Null,
        401,
    )
    .await;
    let events = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/events?limit=50",
        Value::Null,
        200,
    )
    .await;
    let text = events.to_string();
    assert!(text.contains("app.installed") && text.contains("app.uninstalled"));
    assert!(!text.contains(&key) && !text.contains(&secret));
}

#[tokio::test]
async fn the_consent_screen_sends_the_owner_back_to_the_app() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let reg = call(
        &client,
        Some(&t),
        None,
        "POST",
        "/api/apps",
        json!({ "name": "Reviews", "redirect_uris": ["http://localhost:3000/cb"],
                "permissions": ["products:read"] }),
        201,
    )
    .await;
    let client_id = reg["app"]["id"].as_str().unwrap().to_owned();
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let page = client
        .get(&format!(
            "/admin/acme/apps/authorize?client_id={client_id}&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcb&state=s1"
        ))
        .header("cookie", &me)
        .send()
        .await;
    let html = page.text();
    assert_eq!(page.status(), 200, "{html}");
    assert!(
        html.contains("Install Reviews") && html.contains("products:read"),
        "{html}"
    );
    let res = client
        .post("/admin/acme/apps/authorize")
        .header("cookie", &me)
        .form(&json!({ "client_id": client_id, "redirect_uri": "http://localhost:3000/cb", "state": "s1" }))
        .send()
        .await;
    assert_eq!(res.status(), 303, "{}", res.text());
    let to = res.header("location").unwrap();
    assert!(to.starts_with("http://localhost:3000/cb?code="), "{to}");
    assert!(to.ends_with("&shop=acme&state=s1"), "{to}");
    let apps = client
        .get("/admin/acme/apps")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(
        apps.contains("Reviews") && apps.contains("not connected yet"),
        "{apps}"
    );
}

#[tokio::test]
async fn shops_find_listed_apps_in_the_directory() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let mut ids = Vec::new();
    for name in ["Reviews", "Hidden"] {
        let reg = call(
            &client,
            Some(&t),
            None,
            "POST",
            "/api/apps",
            json!({ "name": name, "redirect_uris": ["https://reviews.example.com/cb"],
                    "permissions": ["products:read"] }),
            201,
        )
        .await;
        ids.push(reg["app"]["id"].as_str().unwrap().to_owned());
    }
    let path = format!("/api/apps/{}", ids[0]);

    // Listing needs a tagline and an address to start installing at.
    let e = call(
        &client,
        Some(&t),
        None,
        "PATCH",
        &path,
        json!({ "listed": true }),
        422,
    )
    .await;
    assert_eq!(e["code"], "incomplete_listing");
    for bad in [
        json!({ "app_url": "http://reviews.example.com/install" }),
        json!({ "category": "rockets" }),
    ] {
        call(&client, Some(&t), None, "PATCH", &path, bad, 422).await;
    }
    let app = call(
        &client,
        Some(&t),
        None,
        "PATCH",
        &path,
        json!({ "tagline": "Product reviews with photos", "category": "reviews",
                "app_url": "https://reviews.example.com/install", "listed": true }),
        200,
    )
    .await;
    assert_eq!(app["listed"], true);
    assert_eq!(app["name"], "Reviews");

    // Only the owner changes it.
    call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/staff",
        json!({ "email": "dev@example.com", "password": "dev password 12345" }),
        201,
    )
    .await;
    let session = call(
        &client,
        None,
        None,
        "POST",
        "/api/auth/login",
        json!({ "email": "dev@example.com", "password": "dev password 12345" }),
        200,
    )
    .await;
    let other = session["token"].as_str().unwrap().to_owned();
    call(
        &client,
        Some(&other),
        None,
        "PATCH",
        &path,
        json!({ "listed": false }),
        404,
    )
    .await;

    // Shops find listed apps only.
    let dir = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-directory",
        Value::Null,
        200,
    )
    .await;
    let apps = dir["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 1, "{dir}");
    assert_eq!(apps[0]["name"], "Reviews");
    assert_eq!(
        apps[0]["install_url"],
        "https://reviews.example.com/install?shop=acme"
    );
    assert_eq!(apps[0]["installed"], false);
    assert!(dir["categories"].as_array().unwrap().len() > 3);
    for (q, n) in [("photos", 1), ("nothing+like+it", 0), ("100%25", 0)] {
        let found = call(
            &client,
            Some(&t),
            Some("acme"),
            "GET",
            &format!("/api/app-directory?q={q}"),
            Value::Null,
            200,
        )
        .await;
        assert_eq!(found["apps"].as_array().unwrap().len(), n, "{q}");
    }
    let found = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-directory?category=shipping",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(found["apps"], json!([]));

    // Installed shows as installed, on the API and the admin page.
    call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/app-installations",
        json!({ "client_id": ids[0], "redirect_uri": "https://reviews.example.com/cb" }),
        201,
    )
    .await;
    let dir = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-directory",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(dir["apps"][0]["installed"], true);
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = client
        .get("/admin/acme/apps/directory?q=reviews")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(
        html.contains("Product reviews with photos") && !html.contains("Hidden"),
        "{html}"
    );

    // Unlisting takes it out of the directory.
    call(
        &client,
        Some(&t),
        None,
        "PATCH",
        &path,
        json!({ "listed": false }),
        200,
    )
    .await;
    let dir = call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-directory",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(dir["apps"], json!([]));
}

#[tokio::test]
async fn apps_add_links_to_the_admin() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let reg = call(
        &client,
        Some(&t),
        None,
        "POST",
        "/api/apps",
        json!({ "name": "Reviews", "redirect_uris": ["https://reviews.example.com/cb"],
                "permissions": ["products:read"] }),
        201,
    )
    .await;
    let client_id = reg["app"]["id"].as_str().unwrap().to_owned();
    let secret = reg["client_secret"].as_str().unwrap().to_owned();
    let path = format!("/api/apps/{client_id}");
    for bad in [
        json!({ "admin_links": [{ "target": "rocket", "label": "Go", "url": "https://r.example.com" }] }),
        json!({ "admin_links": [{ "target": "product", "label": "", "url": "https://r.example.com" }] }),
        json!({ "admin_links": [{ "target": "product", "label": "Go", "url": "javascript:alert(1)" }] }),
    ] {
        call(&client, Some(&t), None, "PATCH", &path, bad, 422).await;
    }
    let app = call(
        &client,
        Some(&t),
        None,
        "PATCH",
        &path,
        json!({ "admin_links": [
            { "target": "app", "label": "Open Reviews", "url": "https://reviews.example.com/admin" },
            { "target": "product", "label": "Ask for reviews", "url": "https://reviews.example.com/ask?from=soat" }
        ] }),
        200,
    )
    .await;
    assert_eq!(app["admin_links"].as_array().unwrap().len(), 2);

    // Nothing shows until the app is installed and has its key.
    let links = |target: &'static str| {
        let client = &client;
        let t = &t;
        async move {
            call(
                client,
                Some(t),
                Some("acme"),
                "GET",
                &format!("/api/app-links?target={target}&id=prod_1"),
                Value::Null,
                200,
            )
            .await
        }
    };
    assert_eq!(links("product").await, json!([]));
    let installed = call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/app-installations",
        json!({ "client_id": client_id, "redirect_uri": "https://reviews.example.com/cb" }),
        201,
    )
    .await;
    assert_eq!(links("product").await, json!([]));
    let code = query(installed["redirect_to"].as_str().unwrap(), "code");
    call(
        &client,
        None,
        None,
        "POST",
        "/api/apps/token",
        json!({ "client_id": client_id, "client_secret": secret, "code": code,
                "shop": "acme", "redirect_uri": "https://reviews.example.com/cb" }),
        200,
    )
    .await;
    let product = links("product").await;
    assert_eq!(product.as_array().unwrap().len(), 1, "{product}");
    assert_eq!(product[0]["label"], "Ask for reviews");
    assert_eq!(
        product[0]["url"],
        "https://reviews.example.com/ask?from=soat&shop=acme&id=prod_1"
    );
    assert_eq!(links("order").await, json!([]));
    call(
        &client,
        Some(&t),
        Some("acme"),
        "GET",
        "/api/app-links?target=nowhere",
        Value::Null,
        422,
    )
    .await;

    // The admin shows them on the product's page and the Apps page.
    let p = call(
        &client,
        Some(&t),
        Some("acme"),
        "POST",
        "/api/products",
        json!({ "sku": "TS", "title": "Trail Shoe", "price_minor": 100, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap();
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = client
        .get(&format!("/admin/acme/products/{id}"))
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(
        html.contains(&format!(
            "https://reviews.example.com/ask?from=soat&amp;shop=acme&amp;id={id}"
        )) && html.contains("Ask for reviews"),
        "{html}"
    );
    let html = client
        .get("/admin/acme/apps")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(html.contains("Open Reviews"), "{html}");
}
