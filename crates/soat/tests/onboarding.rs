//! G1: a fresh install set up in the browser, from the first admin to a
//! shop that is ready to sell.

mod common;

use serde_json::{json, Value};
use zapline::test::{TestClient, TestResponse};

async fn get(client: &TestClient, cookie: &str, path: &str) -> TestResponse {
    client.get(path).header("cookie", cookie).send().await
}

async fn post(client: &TestClient, cookie: &str, path: &str, form: Value) -> TestResponse {
    client
        .post(path)
        .header("cookie", cookie)
        .form(&form)
        .send()
        .await
}

#[tokio::test]
async fn a_new_install_gets_ready_to_sell() {
    let (client, _) = common::client_and_url().await;
    client.get("/admin/setup").send().await.assert_status(200);
    let first = json!({
        "setup_token": "wrong", "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD,
    });
    let res = post(&client, "", "/admin/setup", first.clone()).await;
    res.assert_status(403);
    assert!(res
        .text()
        .contains("not the one this server was started with"));

    let mut first = first;
    first["setup_token"] = json!(common::SETUP_TOKEN);
    let res = post(&client, "", "/admin/setup", first.clone()).await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/admin/new"));
    let c = res.cookie("soat_admin").unwrap();
    assert!(c.http_only());
    let me = format!("soat_admin={}", c.value());
    // Only once.
    post(&client, "", "/admin/setup", first)
        .await
        .assert_status(409);

    // An admin without shops is sent to make one.
    let res = get(&client, &me, "/admin").await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/admin/new"));
    get(&client, &me, "/admin/new").await.assert_status(200);
    let res = post(
        &client,
        &me,
        "/admin",
        json!({ "name": "Shop", "id": "login" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("cannot be a shop"));
    let res = post(&client, &me, "/admin", json!({ "name": "Café Ölberg" })).await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/admin/cafe-olberg/welcome"));
    let res = post(
        &client,
        &me,
        "/admin",
        json!({ "name": "Again", "id": "cafe-olberg" }),
    )
    .await;
    res.assert_status(409);
    assert!(res.text().contains("is taken"));

    let welcome = "/admin/cafe-olberg/welcome";
    let html = get(&client, &me, welcome).await.text();
    assert!(html.contains("0 of 5 steps done"), "{html}");
    let html = get(&client, &me, "/admin/cafe-olberg").await.text();
    assert!(html.contains("Finish setting up"), "{html}");

    // Shipping needs to know where from.
    let res = post(
        &client,
        &me,
        &format!("{welcome}/shipping"),
        json!({ "price": "4.95" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("country and currency first"));

    post(
        &client,
        &me,
        &format!("{welcome}/basics"),
        json!({ "name": "Café Ölberg", "country": "AT", "currency": "EUR" }),
    )
    .await
    .assert_status(303);
    let token = client
        .post("/api/auth/login")
        .json(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await
        .json::<Value>()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let api = |path: &'static str| {
        let req = client
            .get(path)
            .header("x-soat-tenant", "cafe-olberg")
            .bearer(&token);
        async move {
            let res = req.send().await;
            res.assert_status(200);
            res.json::<Value>()
        }
    };
    let shop = api("/api/shop").await;
    assert_eq!(shop["country"], "AT");
    assert_eq!(shop["currency"], "EUR");
    assert_eq!(api("/api/tax/settings").await["origin_country"], "AT");
    let html = get(&client, &me, welcome).await.text();
    assert!(
        html.contains("1 of 5 steps done") && html.contains("Load EU VAT rates"),
        "{html}"
    );
    assert!(html.contains("<option value=\"AT\" selected>Austria</option>"));

    post(&client, &me, &format!("{welcome}/taxes"), json!({}))
        .await
        .assert_status(303);
    post(
        &client,
        &me,
        &format!("{welcome}/payments"),
        json!({ "instructions": "IBAN AT00 1234, reference: your order number" }),
    )
    .await
    .assert_status(303);
    post(
        &client,
        &me,
        &format!("{welcome}/shipping"),
        json!({ "price": "5,90", "free_from": "60" }),
    )
    .await
    .assert_status(303);
    let zones = api("/api/shipping/zones").await;
    assert_eq!(zones["items"][0]["name"], "Austria");
    assert_eq!(zones["items"][0]["countries"], json!(["AT"]));
    let mut prices: Vec<i64> = zones["items"][0]["rates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["price_minor"].as_i64().unwrap())
        .collect();
    prices.sort_unstable();
    assert_eq!(prices, [0, 590]);
    let methods = api("/api/payment-methods").await;
    assert_eq!(methods["items"][0]["provider"], "manual");
    assert!(
        api("/api/tax/zones").await["items"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );

    let html = get(&client, &me, welcome).await.text();
    assert!(html.contains("4 of 5 steps done"), "{html}");
    // New products are priced in the shop's currency.
    let html = get(&client, &me, "/admin/cafe-olberg/products/new")
        .await
        .text();
    assert!(html.contains("value=\"EUR\""), "{html}");
    client
        .post("/api/products")
        .header("x-soat-tenant", "cafe-olberg")
        .bearer(&token)
        .json(&json!({ "sku": "MUG", "title": "Mug", "price_minor": 1200, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);
    let html = get(&client, &me, welcome).await.text();
    assert!(html.contains("Your shop is ready to sell"), "{html}");
    let html = get(&client, &me, "/admin/cafe-olberg").await.text();
    assert!(!html.contains("Finish setting up"));
    let html = get(&client, &me, "/admin").await;
    html.assert_status(303);
}

#[tokio::test]
async fn the_shop_profile_belongs_to_its_owner() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let patch = |shop: &'static str, body: Value| {
        client
            .patch("/api/shop")
            .header("x-soat-tenant", shop)
            .bearer(&t)
            .json(&body)
            .send()
    };
    patch("acme", json!({ "country": "de" }))
        .await
        .assert_status(422);
    patch("acme", json!({ "currency": "euro" }))
        .await
        .assert_status(422);
    patch("acme", json!({ "name": " " }))
        .await
        .assert_status(422);
    let res = patch(
        "acme",
        json!({ "name": "Acme GmbH", "country": "DE", "currency": "EUR" }),
    )
    .await;
    res.assert_status(200);
    assert_eq!(res.json::<Value>()["name"], "Acme GmbH");
    // Each shop sees its own profile only.
    let globex = client
        .get("/api/shop")
        .header("x-soat-tenant", "globex")
        .bearer(&t)
        .send()
        .await
        .json::<Value>();
    assert_eq!(globex["id"], "globex");
    assert_eq!(globex["country"], Value::Null);
    let events = client
        .get("/api/events?kind=shop.updated")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .send()
        .await
        .json::<Value>();
    assert_eq!(events["items"].as_array().unwrap().len(), 1);

    // Staff may read the profile but not change it.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "email": "staff@example.com", "password": "a long password", "role": "staff" }))
        .send()
        .await
        .assert_status(201);
    let staff = client
        .post("/api/auth/login")
        .json(&json!({ "email": "staff@example.com", "password": "a long password" }))
        .send()
        .await
        .json::<Value>()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .get("/api/shop")
        .header("x-soat-tenant", "acme")
        .bearer(&staff)
        .send()
        .await
        .assert_status(200);
    client
        .patch("/api/shop")
        .header("x-soat-tenant", "acme")
        .bearer(&staff)
        .json(&json!({ "name": "Mine now" }))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn sample_products_come_and_go() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let welcome = "/admin/acme/welcome";
    let res = post(
        &client,
        &me,
        &format!("{welcome}/sample"),
        json!({ "template": "coffee" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("country and currency first"));
    post(
        &client,
        &me,
        &format!("{welcome}/basics"),
        json!({ "name": "Acme", "country": "CH", "currency": "CHF" }),
    )
    .await
    .assert_status(303);
    let html = get(&client, &me, welcome).await.text();
    assert!(
        html.contains("Load Coffee roastery") && !html.contains("Load EU VAT rates"),
        "{html}"
    );
    post(
        &client,
        &me,
        &format!("{welcome}/sample"),
        json!({ "template": "nope" }),
    )
    .await
    .assert_status(422);
    post(
        &client,
        &me,
        &format!("{welcome}/sample"),
        json!({ "template": "coffee" }),
    )
    .await
    .assert_status(303);

    let api = |path: String| {
        let req = client.get(&path).header("x-soat-tenant", "acme").bearer(&t);
        async move {
            let res = req.send().await;
            res.assert_status(200);
            res.json::<Value>()
        }
    };
    let products = api("/api/products?tag=sample&sort=sku".into()).await;
    assert_eq!(products["total"], 5);
    let eth = products["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["sku"] == "BEAN-ETH")
        .unwrap()
        .clone();
    assert_eq!(eth["currency"], "CHF");
    assert_eq!(eth["price_minor"], 1450);
    let variants = api(format!(
        "/api/products/{}/variants",
        eth["id"].as_str().unwrap()
    ))
    .await;
    let variants = variants
        .as_array()
        .or(variants["items"].as_array())
        .unwrap()
        .clone();
    assert_eq!(variants.len(), 3);
    assert_eq!(variants[0]["sku"], "BEAN-ETH-WHOLE-BEAN");
    let stock = api(format!(
        "/api/inventory/{}",
        variants[1]["id"].as_str().unwrap()
    ))
    .await;
    assert_eq!(stock["available"], 25);
    let locations = api("/api/locations".into()).await;
    assert_eq!(locations["items"][0]["country"], "CH");
    let collections = api("/api/collections?q=sample-coffee".into()).await;
    let coffee = collections["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        api(format!("/api/collections/{coffee}/products")).await["total"],
        5
    );

    // Once at a time.
    let res = post(
        &client,
        &me,
        &format!("{welcome}/sample"),
        json!({ "template": "home" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("loaded already"));
    let html = get(&client, &me, welcome).await.text();
    assert!(html.contains("Remove the sample products"));

    post(&client, &me, &format!("{welcome}/remove-sample"), json!({}))
        .await
        .assert_status(303);
    assert_eq!(api("/api/products?tag=sample".into()).await["total"], 0);
    assert_eq!(api("/api/collections?q=sample-".into()).await["total"], 0);
    // A shop's own products stay.
    client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "sku": "OWN", "title": "Own", "price_minor": 100, "currency": "CHF" }))
        .send()
        .await
        .assert_status(201);
    post(
        &client,
        &me,
        &format!("{welcome}/sample"),
        json!({ "template": "apparel" }),
    )
    .await
    .assert_status(303);
    post(&client, &me, &format!("{welcome}/remove-sample"), json!({}))
        .await
        .assert_status(303);
    assert_eq!(api("/api/products".into()).await["total"], 1);
}

#[tokio::test]
async fn the_launch_checklist_says_what_is_left() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let me = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = get(&client, &me, "/admin/acme/launch").await.text();
    // Basics, products, payments, shipping, taxes and four legal pages.
    assert!(
        html.contains("9 needed before your shop takes real orders"),
        "{html}"
    );
    assert!(html.contains("action=\"/admin/acme/launch/page/imprint\""));

    // A legal page starts as a draft to write.
    let res = post(&client, &me, "/admin/acme/launch/page/imprint", json!({})).await;
    res.assert_status(303);
    let edit = res.header("location").unwrap().to_owned();
    assert!(edit.starts_with("/admin/acme/pages/page_"), "{edit}");
    post(&client, &me, "/admin/acme/launch/page/nonsense", json!({}))
        .await
        .assert_status(404);
    let html = get(&client, &me, "/admin/acme/launch").await.text();
    assert!(
        html.contains("9 needed") && html.contains(&format!("href=\"{edit}\"")),
        "{html}"
    );

    // Published, it counts; so does a page under its German handle.
    let api = |method: &str, path: String, body: Value| {
        let req = match method {
            "PUT" => client.put(&path),
            _ => client.post(&path),
        }
        .header("x-soat-tenant", "acme")
        .bearer(&t);
        async move {
            let res = req.json(&body).send().await;
            assert!(res.status().is_success(), "{}", res.text());
        }
    };
    let id = edit.rsplit('/').next().unwrap();
    api(
        "PUT",
        format!("/api/pages/{id}"),
        json!({ "title": "Imprint", "handle": "imprint", "published": true }),
    )
    .await;
    api(
        "POST",
        "/api/pages".into(),
        json!({ "title": "Datenschutz", "handle": "datenschutz", "published": true }),
    )
    .await;
    let html = get(&client, &me, "/admin/acme/launch").await.text();
    assert!(html.contains("7 needed"), "{html}");
    assert!(!html.contains("launch/page/privacy"));
}
