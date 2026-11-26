//! F10: a shop uploads its own plugin; it runs at that shop's checkout
//! alone, once turned on, and goes away when removed.
#![cfg(feature = "plugins")]

mod common;

use serde_json::{json, Value};
use soat_core::hooks::{Hook, Hooks};
use zapline::db::Database;
use zapline::test::{MultipartForm, TestClient};

const WASM: &[u8] = include_bytes!("../../soat-plugin-host/fixtures/loyalty.wasm");

fn manifest(name: &str, world: &str) -> String {
    format!(
        r#"{{"name":"{name}","version":"1.0.0","world":"{world}","api":"0.1.0",
            "settings":[{{"key":"amount","label":"Amount off"}}]}}"#
    )
}

/// A native hook the install runs, so its name is taken.
struct Sale;
impl Hook for Sale {
    fn name(&self) -> &str {
        "sale"
    }
}

async fn upload(c: &TestClient, t: &str, shop: &str, manifest: &str, wasm: &[u8]) -> (u16, Value) {
    let res = c
        .post("/api/plugins/uploads")
        .header("x-soat-tenant", shop)
        .bearer(t)
        .multipart(MultipartForm::new().text("manifest", manifest).file(
            "component",
            "plugin.wasm",
            "application/wasm",
            wasm.to_vec(),
        ))
        .send()
        .await;
    let status = res.status().as_u16();
    (
        status,
        serde_json::from_str(&res.text()).unwrap_or(Value::Null),
    )
}

async fn call(
    c: &TestClient,
    t: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let req = match method {
        "POST" => c.post(path),
        "PUT" => c.put(path),
        "DELETE" => c.delete(path),
        _ => c.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let status = res.status().as_u16();
    (
        status,
        serde_json::from_str(&res.text()).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_shop_uploads_its_own_plugin() {
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.hooks = Hooks::new().with(Sale);
    let client = soat::app(Database::postgres(url), settings).test();
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "other"]).await;
    let c = &client;

    // A tagged product in stock, and a cart holding it.
    let (_, loc) = call(
        c,
        &t,
        "acme",
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
    )
    .await;
    let (_, p) = call(
        c,
        &t,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": "MUG", "title": "Mug", "price_minor": 1000,
                "currency": "EUR", "tags": ["plugin"] }),
    )
    .await;
    let item = p["id"].as_str().unwrap().to_owned();
    call(
        c,
        &t,
        "acme",
        "POST",
        &format!("/api/inventory/{item}/adjustments"),
        json!({ "location_id": loc["id"], "delta": 10, "reason": "received" }),
    )
    .await;
    let cart: Value = c
        .post("/api/storefront/carts")
        .header("x-soat-tenant", "acme")
        .json(&json!({ "currency": "EUR" }))
        .send()
        .await
        .json();
    let id = cart["cart"]["id"].as_str().unwrap().to_owned();
    let token = cart["token"].as_str().unwrap().to_owned();
    c.post(&format!("/api/storefront/carts/{id}/lines"))
        .header("x-soat-tenant", "acme")
        .header("x-soat-cart-token", &token)
        .json(&json!({ "item_id": item, "quantity": 1 }))
        .send()
        .await
        .assert_status(200);
    let adjustments = || async {
        let res = c
            .patch(&format!("/api/storefront/carts/{id}/checkout"))
            .header("x-soat-tenant", "acme")
            .header("x-soat-cart-token", &token)
            .json(&json!({ "email": "ada@example.com" }))
            .send()
            .await;
        let text = res.text();
        assert_eq!(res.status().as_u16(), 200, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["adjustments"].clone()
    };

    // Uploaded, it is listed for this shop and starts off.
    let (s, saved) = upload(c, &t, "acme", &manifest("loyalty", "discounts"), WASM).await;
    assert_eq!(s, 201, "{saved}");
    assert_eq!(saved["name"], "loyalty");
    assert_eq!(saved["uploaded"], true);
    assert_eq!(saved["version"], "1.0.0");
    assert_eq!(saved["enabled"], false);
    let (_, listed) = call(c, &t, "acme", "GET", "/api/plugins", Value::Null).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["uploaded"], true);
    assert_eq!(adjustments().await, json!([]));

    // Another shop neither sees nor reaches it.
    let (_, theirs) = call(c, &t, "other", "GET", "/api/plugins", Value::Null).await;
    assert_eq!(theirs, json!([]));
    let (s, _) = call(
        c,
        &t,
        "other",
        "PUT",
        "/api/plugins/loyalty",
        json!({ "enabled": true }),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        c,
        &t,
        "other",
        "DELETE",
        "/api/plugins/uploads/loyalty",
        Value::Null,
    )
    .await;
    assert_eq!(s, 404);

    // Turned on, it runs at checkout with the shop's setting.
    let (s, on) = call(
        c,
        &t,
        "acme",
        "PUT",
        "/api/plugins/loyalty",
        json!({ "enabled": true, "settings": { "amount": "300" } }),
    )
    .await;
    assert_eq!(s, 200, "{on}");
    assert_eq!(on["uploaded"], true);
    let got = adjustments().await;
    assert_eq!(got[0]["label"], "Loyalty", "{got}");
    assert_eq!(got[0]["amount_minor"], 300);

    // Uploading it again replaces it and keeps it on, with its settings.
    let (s, again) = upload(c, &t, "acme", &manifest("loyalty", "discounts"), WASM).await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(again["enabled"], true);
    assert_eq!(again["settings"][0]["value"], "300");

    // Refused: an events plugin, not a component, nothing to upload, a
    // name the install uses, a bad name.
    let (s, e) = upload(c, &t, "acme", &manifest("watcher", "events"), WASM).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (422, Some("invalid_plugin")),
        "{e}"
    );
    let (s, e) = upload(c, &t, "acme", &manifest("junk", "discounts"), b"nonsense").await;
    assert_eq!(
        (s, e["code"].as_str()),
        (422, Some("invalid_plugin")),
        "{e}"
    );
    let (s, e) = upload(c, &t, "acme", &manifest("sale", "discounts"), WASM).await;
    assert_eq!((s, e["code"].as_str()), (409, Some("plugin_exists")), "{e}");
    let (s, _) = upload(c, &t, "acme", &manifest("Bad Name", "discounts"), WASM).await;
    assert_eq!(s, 422);
    let res = c
        .post("/api/plugins/uploads")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .multipart(MultipartForm::new().text("manifest", manifest("loyalty", "discounts")))
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 400);

    // The event log says what happened, never with the component.
    let (_, events) = call(
        c,
        &t,
        "acme",
        "GET",
        "/api/events?type=plugin.uploaded",
        Value::Null,
    )
    .await;
    let text = events.to_string();
    assert!(text.contains("plugin.uploaded"), "{text}");
    assert!(text.contains("sha256"), "{text}");

    // The admin's Plugins settings upload and remove them too.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let res = c
        .post("/admin/acme/settings/plugins/upload")
        .header("cookie", &cookie)
        .multipart(
            MultipartForm::new()
                .file(
                    "manifest",
                    "soat.plugin.json",
                    "application/json",
                    manifest("points", "discounts").into_bytes(),
                )
                .file(
                    "component",
                    "plugin.wasm",
                    "application/wasm",
                    WASM.to_vec(),
                ),
        )
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/acme/settings/plugins?done=1"),
        "{}",
        res.text()
    );
    let html = c
        .get("/admin/acme/settings/plugins")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("points") && html.contains("Uploaded"),
        "{html}"
    );
    assert!(html.contains("Upload a plugin"), "{html}");
    let res = c
        .post("/admin/acme/settings/plugins/upload")
        .header("cookie", &cookie)
        .multipart(MultipartForm::new().file(
            "manifest",
            "soat.plugin.json",
            "application/json",
            manifest("junk", "discounts").into_bytes(),
        ))
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 422);
    assert!(res.text().contains("Choose the plugin"));
    let res = c
        .post("/admin/acme/settings/plugins/remove/points")
        .header("cookie", &cookie)
        .form(&json!({}))
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/acme/settings/plugins?done=1")
    );

    // Removed: gone from the list and from checkout.
    let (s, _) = call(
        c,
        &t,
        "acme",
        "DELETE",
        "/api/plugins/uploads/loyalty",
        Value::Null,
    )
    .await;
    assert_eq!(s, 204);
    let (_, listed) = call(c, &t, "acme", "GET", "/api/plugins", Value::Null).await;
    assert_eq!(listed, json!([]));
    assert_eq!(adjustments().await, json!([]));
}
