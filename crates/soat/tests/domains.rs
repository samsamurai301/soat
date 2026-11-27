//! Shop resolution: by API key, by header, or by the request's domain.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn add(client: &TestClient, token: &str, shop: &str, domain: &str, status: u16) {
    client
        .post("/api/domains")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "domain": domain }))
        .send()
        .await
        .assert_status(status);
}

async fn product(client: &TestClient, token: &str, shop: &str, sku: &str) {
    client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": sku, "title": sku, "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);
}

async fn skus_on(client: &TestClient, token: &str, host: &str) -> Vec<String> {
    let page: Value = client
        .get("/api/products")
        .header("host", host)
        .bearer(token)
        .send()
        .await
        .assert_status(200)
        .json();
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["sku"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_host_picks_the_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add(&client, &admin, "acme", "Shop.Acme.example.", 201).await;
    add(&client, &admin, "globex", "globex.example", 201).await;
    product(&client, &admin, "acme", "A").await;
    product(&client, &admin, "globex", "G").await;

    assert_eq!(skus_on(&client, &admin, "shop.acme.example").await, ["A"]);
    assert_eq!(skus_on(&client, &admin, "globex.example:8080").await, ["G"]);

    // An unknown host is the same as no shop at all.
    client
        .get("/api/products")
        .header("host", "nobody.example")
        .bearer(&admin)
        .send()
        .await
        .assert_status(400);
    // The header wins over the host.
    let page: Value = client
        .get("/api/products")
        .header("host", "globex.example")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(page["items"][0]["sku"], "A");
}

#[tokio::test]
async fn a_domain_belongs_to_one_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    add(&client, &admin, "acme", "acme.example", 201).await;
    add(&client, &admin, "globex", "ACME.example", 409).await;
    add(&client, &admin, "globex", "localhost", 422).await;
    add(&client, &admin, "globex", "10.0.0.1", 422).await;

    // Another shop cannot see or remove it.
    let theirs: Value = client
        .get("/api/domains")
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(theirs, json!([]));
    client
        .delete("/api/domains/acme.example")
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);

    client
        .delete("/api/domains/acme.example")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    add(&client, &admin, "globex", "acme.example", 201).await;
}

#[tokio::test]
async fn a_host_does_not_let_strangers_in() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    add(&client, &admin, "acme", "acme.example", 201).await;
    client
        .post("/api/tenants")
        .bearer(&admin)
        .json(&json!({ "id": "other", "name": "Other" }))
        .send()
        .await
        .assert_status(201);
    // Make an owner of `other` who is not in `acme`.
    client
        .post("/api/staff")
        .header("x-soat-tenant", "other")
        .bearer(&admin)
        .json(&json!({ "email": "stranger@example.com", "password": "stranger password", "role": "owner" }))
        .send()
        .await
        .assert_status(201);
    let login: Value = client
        .post("/api/auth/login")
        .json(&json!({ "email": "stranger@example.com", "password": "stranger password" }))
        .send()
        .await
        .assert_status(200)
        .json();
    client
        .get("/api/products")
        .header("host", "acme.example")
        .bearer(login["token"].as_str().unwrap())
        .send()
        .await
        .assert_status(404);
}

/// TXT records as a test's DNS has them.
#[derive(Default)]
struct FakeDns(std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>);

impl soat_tenancy::domains::TxtLookup for FakeDns {
    fn txt<'a>(&'a self, name: &'a str) -> soat_tenancy::domains::TxtFuture<'a> {
        let found = self
            .0
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default();
        Box::pin(async move { Ok(found) })
    }
}

#[tokio::test]
async fn shops_prove_they_own_a_domain_before_it_serves_them() {
    let dns = std::sync::Arc::new(FakeDns::default());
    let settings = soat::Settings {
        domain_proof: Some(dns.clone()),
        ..soat::Settings::for_tests(common::SETUP_TOKEN)
    };
    let url = common::fresh_database_url().await;
    let client = soat::app(zapline::db::Database::postgres(url), settings).test();
    let admin = common::setup_admin(&client).await;
    common::create_tenants(&client, &admin, &["acme", "squatter"]).await;
    product(&client, &admin, "acme", "A").await;
    let call = |shop: &'static str, method: &'static str, path: String, body: Value| {
        let req = match method {
            "POST" => client.post(&path),
            "DELETE" => client.delete(&path),
            _ => client.get(&path),
        }
        .header("x-soat-tenant", shop)
        .bearer(&admin);
        async move {
            let req = if body.is_null() { req } else { req.json(&body) };
            let res = req.send().await;
            let status = res.status().as_u16();
            let text = res.text();
            (
                status,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
            )
        }
    };

    // Both claim it; neither is served yet.
    let (status, mine) = call(
        "acme",
        "POST",
        "/api/domains".into(),
        json!({ "domain": "acme.example" }),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(mine["verified"], false);
    assert_eq!(mine["txt_name"], "_soat-challenge.acme.example");
    let value = mine["txt_value"].as_str().unwrap().to_owned();
    assert!(value.starts_with("soat-verify="));
    let (status, theirs) = call(
        "squatter",
        "POST",
        "/api/domains".into(),
        json!({ "domain": "acme.example" }),
    )
    .await;
    assert_eq!(status, 201);
    assert_ne!(theirs["txt_value"], mine["txt_value"]);
    let (_, again) = call(
        "acme",
        "POST",
        "/api/domains".into(),
        json!({ "domain": "acme.example" }),
    )
    .await;
    assert_eq!(again["txt_value"], value, "asking again keeps the token");
    client
        .get("/api/products")
        .header("host", "acme.example")
        .bearer(&admin)
        .send()
        .await
        .assert_status(400);

    // Without the record, verifying says what DNS has.
    let (status, e) = call(
        "acme",
        "POST",
        "/api/domains/acme.example/verify".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(e["code"], "not_verified");
    assert!(
        e["error"]
            .as_str()
            .unwrap_or(&e.to_string())
            .contains("no TXT record"),
        "{e}"
    );

    // The owner adds the record: the domain is theirs, the squatter's claim
    // can no longer win.
    dns.0.lock().unwrap().insert(
        "_soat-challenge.acme.example".into(),
        vec!["something else".into(), value.clone()],
    );
    let (status, _) = call(
        "squatter",
        "POST",
        "/api/domains/acme.example/verify".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, 422, "the record names acme's token, not theirs");
    let (status, done) = call(
        "acme",
        "POST",
        "/api/domains/acme.example/verify".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["verified"], true);
    assert_eq!(skus_on(&client, &admin, "acme.example").await, ["A"]);
    let (_, list) = call("acme", "GET", "/api/domains".into(), Value::Null).await;
    assert_eq!(
        list,
        json!([{ "domain": "acme.example", "verified": true, "txt_name": null, "txt_value": null }])
    );
    let (status, e) = call(
        "squatter",
        "POST",
        "/api/domains".into(),
        json!({ "domain": "acme.example" }),
    )
    .await;
    assert_eq!((status, e["code"].as_str()), (409, Some("domain_taken")));
    let (_, events) = call(
        "acme",
        "GET",
        "/api/events?kind=domain.added".into(),
        Value::Null,
    )
    .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 1);

    // A claim can be dropped; a domain the shop never asked for is a 404.
    let (status, _) = call(
        "squatter",
        "DELETE",
        "/api/domains/acme.example".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = call(
        "squatter",
        "POST",
        "/api/domains/acme.example/verify".into(),
        Value::Null,
    )
    .await;
    assert_eq!(status, 404);
}
