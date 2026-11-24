//! B3: smart collections, filled by rules and kept up to date.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

struct Shop {
    client: TestClient,
    token: String,
}

impl Shop {
    async fn call(&self, method: &str, path: &str, body: Value, status: u16) -> Value {
        let req = match method {
            "POST" => self.client.post(path),
            "PATCH" => self.client.patch(path),
            "PUT" => self.client.put(path),
            _ => self.client.get(path),
        }
        .header("x-soat-tenant", "acme")
        .bearer(&self.token);
        let req = if body.is_null() { req } else { req.json(&body) };
        let res = req.send().await;
        res.assert_status(status);
        if res.text().is_empty() {
            Value::Null
        } else {
            res.json()
        }
    }

    async fn product(&self, sku: &str, price: i64, tags: &[&str]) -> String {
        let p = self
            .call(
                "POST",
                "/api/products",
                json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR", "tags": tags }),
                201,
            )
            .await;
        p["id"].as_str().unwrap().to_owned()
    }

    async fn members(&self, collection: &str) -> Vec<String> {
        let page = self
            .call(
                "GET",
                &format!("/api/collections/{collection}/products"),
                Value::Null,
                200,
            )
            .await;
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["sku"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[tokio::test]
async fn rules_pick_products_and_keep_picking() {
    let (client, token) = common::with_tenants(&["acme", "globex"]).await;
    let s = Shop { client, token };
    s.product("HAT", 1500, &["Summer"]).await;
    let tee = s.product("TEE", 2900, &["summer", "cotton"]).await;
    s.product("COAT", 19900, &["winter"]).await;

    for bad in [
        json!([{ "field": "vendor", "op": "equals", "value": "x" }]),
        json!([{ "field": "price_minor", "op": "less_than", "value": "50.00" }]),
        json!([{ "field": "tag", "op": "contains", "value": "sum" }]),
    ] {
        let e = s
            .call(
                "POST",
                "/api/collections",
                json!({ "title": "Bad", "rules": bad }),
                422,
            )
            .await;
        assert_eq!(e["code"], "invalid_rules");
    }

    let c = s
        .call(
            "POST",
            "/api/collections",
            json!({ "title": "Summer under 50", "rules": [
                { "field": "tag", "op": "equals", "value": "summer" },
                { "field": "price_minor", "op": "less_than", "value": "5000" },
            ] }),
            201,
        )
        .await;
    let id = c["id"].as_str().unwrap().to_owned();
    assert_eq!(c["match_all"], true);
    assert_eq!(c["rules"][1]["value"], "5000");
    assert_eq!(s.members(&id).await, ["HAT", "TEE"]);

    // Products move in and out as they change, and new ones join.
    s.call(
        "PATCH",
        &format!("/api/products/{tee}"),
        json!({ "price_minor": 6900 }),
        200,
    )
    .await;
    assert_eq!(s.members(&id).await, ["HAT"]);
    s.product("CAP", 900, &["summer"]).await;
    s.call(
        "PATCH",
        &format!("/api/products/{tee}"),
        json!({ "price_minor": 2500 }),
        200,
    )
    .await;
    assert_eq!(s.members(&id).await, ["HAT", "CAP", "TEE"]);

    // Rules fill it, not hands.
    let e = s
        .call(
            "PUT",
            &format!("/api/collections/{id}/products/{tee}"),
            Value::Null,
            409,
        )
        .await;
    assert_eq!(e["code"], "smart_collection");

    // Any rule instead of all; then other rules.
    s.call(
        "PATCH",
        &format!("/api/collections/{id}"),
        json!({ "match_all": false }),
        200,
    )
    .await;
    assert_eq!(s.members(&id).await, ["CAP", "HAT", "TEE"]);
    s.call(
        "PATCH",
        &format!("/api/collections/{id}"),
        json!({ "rules": [
            { "field": "title", "op": "not_contains", "value": "a" },
            { "field": "tag", "op": "not_equals", "value": "winter" },
        ], "match_all": true }),
        200,
    )
    .await;
    assert_eq!(s.members(&id).await, ["TEE"]);

    // No rules: picked by hand again, keeping its products.
    let c = s
        .call(
            "PATCH",
            &format!("/api/collections/{id}"),
            json!({ "rules": [] }),
            200,
        )
        .await;
    assert!(c["rules"].is_null());
    let hat = s.call("GET", "/api/products?q=HAT", Value::Null, 200).await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.call(
        "PUT",
        &format!("/api/collections/{id}/products/{hat}"),
        Value::Null,
        204,
    )
    .await;
    assert_eq!(s.members(&id).await, ["TEE", "HAT"]);

    // Another shop's products never join.
    let other = s
        .client
        .post("/api/products")
        .header("x-soat-tenant", "globex")
        .bearer(&s.token)
        .json(&json!({ "sku": "TEE", "title": "TEE", "price_minor": 100, "currency": "EUR", "tags": ["summer"] }))
        .send()
        .await;
    other.assert_status(201);
    let summer = s
        .call(
            "POST",
            "/api/collections",
            json!({ "title": "All summer", "rules": [{ "field": "tag", "op": "equals", "value": "SUMMER" }] }),
            201,
        )
        .await;
    assert_eq!(
        s.members(summer["id"].as_str().unwrap()).await,
        ["CAP", "HAT", "TEE"]
    );
}

#[tokio::test]
async fn staff_make_collections_in_the_admin() {
    let (client, token) = common::with_tenants(&["acme"]).await;
    let s = Shop { client, token };
    s.product("HAT", 1500, &["summer"]).await;
    s.product("TEE", 2900, &["summer"]).await;
    s.product("COAT", 19900, &["winter"]).await;
    let res = s
        .client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let post = |path: String, form: Value| {
        s.client
            .post(&path)
            .header("cookie", &cookie)
            .form(&form)
            .send()
    };
    let get = |path: String| s.client.get(&path).header("cookie", &cookie).send();

    assert!(get("/admin/acme/collections".into())
        .await
        .text()
        .contains("No collections yet"));
    // A smart collection without rules is explained.
    let res = post(
        "/admin/acme/collections".into(),
        json!({ "title": "Cheap summer", "kind": "smart", "field0": "tag", "op0": "equals", "value0": "" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("at least one rule"));

    let res = post(
        "/admin/acme/collections".into(),
        json!({ "title": "Cheap summer", "kind": "smart", "match": "all",
                "field0": "tag", "op0": "equals", "value0": "summer",
                "field1": "price_minor", "op1": "less_than", "value1": "20,00" }),
    )
    .await;
    res.assert_status(303);
    let smart = res.header("location").unwrap().to_owned();
    let html = get(smart.clone()).await.text();
    assert!(html.contains("HAT") && !html.contains("TEE") && !html.contains("COAT"));
    assert!(html.contains("value=\"20.00\""));
    assert!(!html.contains("Add by SKU"));

    // By hand: add by SKU, order, remove.
    let res = post(
        "/admin/acme/collections".into(),
        json!({ "title": "Picks", "kind": "manual" }),
    )
    .await;
    let picks = res.header("location").unwrap().to_owned();
    for sku in ["tee", "COAT"] {
        post(format!("{picks}/add"), json!({ "sku": sku }))
            .await
            .assert_status(303);
    }
    let res = post(format!("{picks}/add"), json!({ "sku": "NOPE" })).await;
    res.assert_status(422);
    assert!(res.text().contains("No product has the SKU NOPE"));
    let id = picks.rsplit('/').next().unwrap();
    let m = members(&s, id).await;
    assert_eq!(m[0].0, "TEE");
    post(format!("{picks}/up"), json!({ "product": m[1].1 }))
        .await
        .assert_status(303);
    assert_eq!(members(&s, id).await[0].0, "COAT");
    post(format!("{picks}/remove"), json!({ "product": m[1].1 }))
        .await
        .assert_status(303);
    assert_eq!(members(&s, id).await.len(), 1);

    let list = get("/admin/acme/collections".into()).await.text();
    assert!(list.contains("Cheap summer") && list.contains("Smart"));
    post(format!("{picks}/delete"), json!({}))
        .await
        .assert_status(303);
    assert!(!get("/admin/acme/collections".into())
        .await
        .text()
        .contains("Picks"));
}

async fn members(s: &Shop, id: &str) -> Vec<(String, String)> {
    s.call(
        "GET",
        &format!("/api/collections/{id}/products"),
        Value::Null,
        200,
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["sku"].as_str().unwrap().to_owned(),
                p["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
