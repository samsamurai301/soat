//! F3: an install's hooks at checkout: shipping filtered, amounts taken
//! off, orders blocked.

mod common;

use serde_json::{json, Value};
use soat_core::hooks::{Adjustment, Hook, HookCart, HookShippingOption, Hooks, Verdict};
use zapline::db::Database;
use zapline::test::TestClient;

/// 1 off every unit of a product tagged `sale`, then 0.50 off carts of
/// 10.00 or more.
struct Sale;
impl Hook for Sale {
    fn name(&self) -> &str {
        "sale"
    }
    fn discount(&self, _: &str, cart: &HookCart) -> Vec<Adjustment> {
        let mut out: Vec<Adjustment> = cart
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.tags.iter().any(|t| t == "sale"))
            .map(|(i, l)| Adjustment {
                line: Some(i as u32),
                amount_minor: 50 * l.quantity,
                label: "Sale".into(),
            })
            .collect();
        if cart.subtotal_minor >= 1000 {
            out.push(Adjustment {
                line: None,
                amount_minor: 50,
                label: "Welcome".into(),
            });
        }
        // Asking for more than the cart holds is kept within it.
        out.push(Adjustment {
            line: Some(99),
            amount_minor: 1_000_000,
            label: "Nonsense".into(),
        });
        out
    }
}

/// Free shipping from 30.00; no express; tries to raise a price too.
struct FreeShipping;
impl Hook for FreeShipping {
    fn name(&self) -> &str {
        "free-shipping"
    }
    fn filter_shipping(
        &self,
        _: &str,
        cart: &HookCart,
        options: &[HookShippingOption],
    ) -> Vec<HookShippingOption> {
        options
            .iter()
            .filter(|o| o.name != "Express")
            .map(|o| HookShippingOption {
                price_minor: if cart.subtotal_minor >= 3000 {
                    0
                } else {
                    o.price_minor * 10
                },
                ..o.clone()
            })
            .collect()
    }
}

/// No orders for one domain.
struct NoBlocked;
impl Hook for NoBlocked {
    fn name(&self) -> &str {
        "no-blocked"
    }
    fn validate(&self, shop: &str, cart: &HookCart) -> Verdict {
        assert_eq!(shop, "acme");
        if cart.lines.iter().any(|l| l.quantity > 3) {
            Verdict::Block("At most three of each".into())
        } else {
            Verdict::Allow
        }
    }
}

async fn staff(c: &TestClient, t: &str, method: &str, path: &str, body: Value) -> Value {
    let req = match method {
        "POST" => c.post(path),
        "PUT" => c.put(path),
        _ => c.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(t);
    let res = req.json(&body).send().await;
    let text = res.text();
    assert!(res.status().as_u16() < 300, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

async fn shopper(
    c: &TestClient,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let req = match method {
        "POST" => c.post(path),
        "PATCH" => c.patch(path),
        _ => c.get(path),
    }
    .header("x-soat-tenant", "acme")
    .header("x-soat-cart-token", token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    (
        res.status().as_u16(),
        serde_json::from_str(&text).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn hooks_shape_the_checkout() {
    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.hooks = Hooks::new().with(Sale).with(FreeShipping).with(NoBlocked);
    let client = soat::app(Database::postgres(url), settings).test();
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme"]).await;
    let c = &client;

    let loc = staff(
        c,
        &t,
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
    )
    .await;
    let mut items = Vec::new();
    for (sku, price, tags) in [("TEE", 1190, json!(["sale"])), ("BOOK", 1070, json!([]))] {
        let p = staff(
            c,
            &t,
            "POST",
            "/api/products",
            json!({ "sku": sku, "title": sku,
            "price_minor": price, "currency": "EUR", "tags": tags }),
        )
        .await;
        let id = p["id"].as_str().unwrap().to_owned();
        staff(
            c,
            &t,
            "POST",
            &format!("/api/inventory/{id}/adjustments"),
            json!({ "location_id": loc["id"], "delta": 10, "reason": "received" }),
        )
        .await;
        items.push(id);
    }
    staff(
        c,
        &t,
        "PUT",
        "/api/tax/settings",
        json!({ "origin_country": "DE",
        "prices_include_tax": true, "charge_destination_vat": false }),
    )
    .await;
    let z = staff(
        c,
        &t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": "DE", "countries": ["DE"] }),
    )
    .await;
    let rates = format!("/api/shipping/zones/{}/rates", z["id"].as_str().unwrap());
    let standard = staff(
        c,
        &t,
        "POST",
        &rates,
        json!({ "name": "Standard", "currency": "EUR", "price_minor": 495 }),
    )
    .await;
    staff(
        c,
        &t,
        "POST",
        &rates,
        json!({ "name": "Express", "currency": "EUR", "price_minor": 995 }),
    )
    .await;
    let invoice = staff(
        c,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual",
        "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
    )
    .await;

    let cart: Value = {
        let res = c
            .post("/api/storefront/carts")
            .header("x-soat-tenant", "acme")
            .json(&json!({ "currency": "EUR" }))
            .send()
            .await;
        res.json()
    };
    let id = cart["cart"]["id"].as_str().unwrap().to_owned();
    let token = cart["token"].as_str().unwrap().to_owned();
    for (item, q) in [(&items[0], 2), (&items[1], 1)] {
        let (s, _) = shopper(
            c,
            &token,
            "POST",
            &format!("/api/storefront/carts/{id}/lines"),
            json!({ "item_id": item, "quantity": q }),
        )
        .await;
        assert_eq!(s, 200);
    }
    let path = format!("/api/storefront/carts/{id}/checkout");
    let berlin = json!({ "first_name": "Ada", "line1": "Hauptstr. 1", "postal_code": "10115",
        "city": "Berlin", "country": "DE" });
    let (s, view) = shopper(
        c,
        &token,
        "PATCH",
        &path,
        json!({ "email": "ada@example.com",
        "shipping_address": berlin, "shipping_rate_id": standard["id"] }),
    )
    .await;
    assert_eq!(s, 200, "{view}");

    // Express is hidden; Standard is free from 30.00 and never dearer.
    let options = view["shipping_options"].as_array().unwrap();
    assert_eq!(options.len(), 1, "{view}");
    assert_eq!(options[0]["name"], "Standard");
    assert_eq!(options[0]["price_minor"], 0);
    assert_eq!(view["shipping_minor"], 0);

    // 1.00 off the two tees, then 0.50 over the cart; nonsense dropped.
    let adjustments = view["adjustments"].as_array().unwrap();
    assert_eq!(adjustments.len(), 2, "{view}");
    assert_eq!(adjustments[0]["amount_minor"], 100);
    assert_eq!(adjustments[1]["label"], "Welcome");
    assert_eq!(view["discount_minor"], 150);
    assert_eq!(view["total_minor"], 3450 - 150);

    // Four tees is blocked, with the hook's reason.
    let (s, _) = shopper(
        c,
        &token,
        "POST",
        &format!("/api/storefront/carts/{id}/lines"),
        json!({ "item_id": items[0], "quantity": 2 }),
    )
    .await;
    assert_eq!(s, 200);
    let (s, e) = shopper(
        c,
        &token,
        "POST",
        &format!("{path}/complete"),
        json!({ "payment_method_id": invoice["id"] }),
    )
    .await;
    assert_eq!(s, 422, "{e}");
    assert_eq!(e["code"], "order_blocked");
    assert!(e.to_string().contains("At most three of each"));

    // Back to two, the order is placed with the hooks' amounts in it.
    let (_, view) = shopper(c, &token, "GET", &path, Value::Null).await;
    let tee_line = view["cart"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["item_id"] == items[0])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (s, _) = shopper(
        c,
        &token,
        "PATCH",
        &format!("/api/storefront/carts/{id}/lines/{tee_line}"),
        json!({ "quantity": 2 }),
    )
    .await;
    assert_eq!(s, 200);
    let (s, done) = shopper(
        c,
        &token,
        "POST",
        &format!("{path}/complete"),
        json!({ "payment_method_id": invoice["id"] }),
    )
    .await;
    assert_eq!(s, 201, "{done}");
    assert_eq!(done["order"]["total_minor"], 3300);
    assert_eq!(done["order"]["shipping_minor"], 0);
}

/// F2 and F9: a WebAssembly plugin runs at checkout for the shops that
/// turn it on, with their settings; secret ones are never shown.
#[cfg(feature = "plugins")]
#[tokio::test]
async fn a_shop_turns_on_a_wasm_plugin() {
    let wasm = include_bytes!("../../soat-plugin-host/fixtures/loyalty.wasm");
    let manifest = r#"{"name":"loyalty","version":"1.0.0","world":"discounts","api":"0.1.0",
        "settings":[{"key":"amount","label":"Amount off"},
                    {"key":"token","label":"Token","secret":true}]}"#;
    let plugin = soat_plugin_host::PluginHost::new()
        .unwrap()
        .load(manifest, wasm, Default::default())
        .unwrap();

    let url = common::fresh_database_url().await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.hooks = Hooks::new().with(plugin);
    let client = soat::app(Database::postgres(url), settings).test();
    let t = common::setup_admin(&client).await;
    common::create_tenants(&client, &t, &["acme", "other"]).await;
    let c = &client;

    let loc = staff(
        c,
        &t,
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
    )
    .await;
    let p = staff(
        c,
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "MUG", "title": "Mug", "price_minor": 1000,
        "currency": "EUR", "tags": ["plugin"] }),
    )
    .await;
    let item = p["id"].as_str().unwrap().to_owned();
    staff(
        c,
        &t,
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
    let (s, _) = shopper(
        c,
        &token,
        "POST",
        &format!("/api/storefront/carts/{id}/lines"),
        json!({ "item_id": item, "quantity": 1 }),
    )
    .await;
    assert_eq!(s, 200);
    let checkout = || async {
        let (s, view) = shopper(
            c,
            &token,
            "PATCH",
            &format!("/api/storefront/carts/{id}/checkout"),
            json!({ "email": "ada@example.com" }),
        )
        .await;
        assert_eq!(s, 200, "{view}");
        view
    };

    // Offered, but off until the shop turns it on.
    let listed = staff(c, &t, "GET", "/api/plugins", Value::Null).await;
    assert_eq!(listed[0]["name"], "loyalty");
    assert_eq!(listed[0]["enabled"], false);
    assert_eq!(listed[0]["settings"][1]["secret"], true);
    assert_eq!(checkout().await["adjustments"], json!([]));

    let unknown = c
        .put("/api/plugins/loyalty")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "enabled": true, "settings": { "nope": "1" } }))
        .send()
        .await;
    assert_eq!(unknown.status().as_u16(), 422);
    let missing = c
        .put("/api/plugins/nothing")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "enabled": true }))
        .send()
        .await;
    assert_eq!(missing.status().as_u16(), 404);

    let saved = staff(
        c,
        &t,
        "PUT",
        "/api/plugins/loyalty",
        json!({ "enabled": true, "settings": { "amount": "300", "token": "s3cret-value" } }),
    )
    .await;
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["settings"][0]["value"], "300");
    assert_eq!(saved["settings"][1]["value"], Value::Null);
    assert_eq!(saved["settings"][1]["set"], true);
    assert!(!saved.to_string().contains("s3cret"));

    // 3.00 off the tagged line from the shop's setting; the oversized ask
    // is cut to what the cart still holds.
    let view = checkout().await;
    let adjustments = view["adjustments"].as_array().unwrap();
    assert_eq!(adjustments.len(), 2, "{view}");
    assert_eq!(adjustments[0]["label"], "Loyalty");
    assert_eq!(adjustments[0]["amount_minor"], 300);
    assert_eq!(adjustments[1]["amount_minor"], 700);
    assert_eq!(view["discount_minor"], 1000);

    // Changing a plain setting keeps the secret.
    let saved = staff(
        c,
        &t,
        "PUT",
        "/api/plugins/loyalty",
        json!({ "enabled": true, "settings": { "amount": "200" } }),
    )
    .await;
    assert_eq!(saved["settings"][1]["set"], true);
    assert_eq!(checkout().await["adjustments"][0]["amount_minor"], 200);

    // The admin's Plugins settings set it up the same way; an empty
    // secret keeps the one stored.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/settings/plugins")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("loyalty") && html.contains("Amount off"),
        "{html}"
    );
    assert!(html.contains("value=\"200\"") && !html.contains("s3cret"));
    let res = c
        .post("/admin/acme/settings/plugins/save/loyalty")
        .header("cookie", &cookie)
        .form(&json!({ "enabled": "1", "setting_amount": "150", "setting_token": "" }))
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/acme/settings/plugins?done=1")
    );
    let listed = staff(c, &t, "GET", "/api/plugins", Value::Null).await;
    assert_eq!(listed[0]["settings"][0]["value"], "150");
    assert_eq!(listed[0]["settings"][1]["set"], true);

    // Another shop has it off.
    let res = c
        .get("/api/plugins")
        .header("x-soat-tenant", "other")
        .bearer(&t)
        .send()
        .await;
    let other: Value = res.json();
    assert_eq!(other[0]["enabled"], false);
    assert_eq!(other[0]["settings"][0]["set"], false);

    // Off again: nothing taken.
    staff(
        c,
        &t,
        "PUT",
        "/api/plugins/loyalty",
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(checkout().await["adjustments"], json!([]));
}
