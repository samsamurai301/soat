//! I7: subscriptions. Staff give a product plans; customers subscribe,
//! pause, skip, resume and cancel; renewals place orders by themselves.
#![cfg(feature = "subscriptions")]

mod common;

use serde_json::{json, Value};
use zapline::db::sqlx::{self, PgPool};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

async fn register(c: &TestClient, shop: &str, email: &str) -> String {
    let s = call(
        c,
        shop,
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": email, "password": "a long password" }),
        201,
    )
    .await;
    s["token"].as_str().unwrap().to_owned()
}

/// Makes every subscription of `shop` due now.
async fn make_due(pool: &PgPool, shop: &str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', $1, true)")
        .bind(shop)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE subscriptions SET next_order_at = now() - interval '1 minute'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn act(c: &TestClient, token: &str, id: &str, action: &str, status: u16) -> Value {
    call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/subscriptions/{id}/actions"),
        Some(token),
        json!({ "action": action }),
        status,
    )
    .await
}

#[tokio::test]
async fn customers_subscribe_and_renewals_place_orders() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let res = c.resources().await;
    let pool: PgPool = res.require().unwrap();

    let coffee = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "BEANS", "title": "Coffee beans", "price_minor": 2000,
                "currency": "EUR" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let loc = call(
        c,
        "acme",
        "POST",
        "/api/locations",
        a,
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/inventory/{coffee}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 10, "reason": "received" }),
        200,
    )
    .await;

    // Staff add a plan: every 2 weeks, 10% off, 4.90 shipping.
    call(
        c,
        "acme",
        "POST",
        "/api/subscription-plans",
        a,
        json!({ "product_id": coffee, "name": "Every 2 weeks", "interval_unit": "day",
                "interval_count": 2 }),
        422,
    )
    .await;
    let plan = call(
        c,
        "acme",
        "POST",
        "/api/subscription-plans",
        a,
        json!({ "product_id": coffee, "name": "Every 2 weeks", "interval_unit": "week",
                "interval_count": 2, "discount_bps": 1000, "shipping_price_minor": 490 }),
        201,
    )
    .await;
    let plan_id = plan["id"].as_str().unwrap().to_owned();
    let offered = call(
        c,
        "acme",
        "GET",
        &format!("/api/storefront/products/{coffee}/subscription-plans"),
        None,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(offered["items"][0]["id"], plan_id.as_str());

    // Ada subscribes; without an address she is told to give one.
    let ada = register(c, "acme", "ada@example.com").await;
    call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/subscriptions",
        Some(&ada),
        json!({ "plan_id": plan_id, "quantity": 2 }),
        422,
    )
    .await;
    let done = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/subscriptions",
        Some(&ada),
        json!({ "plan_id": plan_id, "quantity": 2,
                "shipping_address": { "first_name": "Ada", "last_name": "L",
                    "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin",
                    "country": "DE" } }),
        201,
    )
    .await;
    let sub = done["subscription"].clone();
    let sub_id = sub["id"].as_str().unwrap().to_owned();
    assert_eq!(sub["status"], "active");
    assert_eq!(sub["orders_count"], 1);
    assert_eq!(sub["title"], "Coffee beans");
    let first = done["order_id"].as_str().unwrap().to_owned();
    assert_eq!(sub["last_order_id"], first.as_str());
    let order = call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{first}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(order["subtotal_minor"], 4000);
    assert_eq!(order["discount_minor"], 400);
    assert_eq!(order["shipping_minor"], 490);

    // Nothing is due yet.
    assert_eq!(soat_subscriptions::renew_due(&res).await.unwrap(), 0);

    // Skipping moves the next order two weeks on; pausing stops renewals.
    let skipped = act(c, &ada, &sub_id, "skip", 200).await;
    assert!(skipped["next_order_at"].as_str().unwrap() > sub["next_order_at"].as_str().unwrap());
    act(c, &ada, &sub_id, "pause", 200).await;
    act(c, &ada, &sub_id, "pause", 409).await;
    make_due(&pool, "acme").await;
    assert_eq!(soat_subscriptions::renew_due(&res).await.unwrap(), 0);
    act(c, &ada, &sub_id, "resume", 200).await;
    act(c, &ada, &sub_id, "fly", 422).await;

    // Due again, the job places the second order once.
    make_due(&pool, "acme").await;
    assert_eq!(soat_subscriptions::renew_due(&res).await.unwrap(), 1);
    assert_eq!(soat_subscriptions::renew_due(&res).await.unwrap(), 0);
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/subscriptions",
        Some(&ada),
        Value::Null,
        200,
    )
    .await;
    let s = &mine["items"][0];
    assert_eq!(s["orders_count"], 2);
    let second = s["last_order_id"].as_str().unwrap();
    assert_ne!(second, first);
    let order = call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{second}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(order["discount_minor"], 400);
    assert_eq!(order["customer_id"], sub["customer_id"]);

    // Staff see it; an inactive plan takes no new subscribers.
    let listed = call(
        c,
        "acme",
        "GET",
        "/api/subscriptions?status=active",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/subscription-plans/{plan_id}"),
        a,
        json!({ "active": false }),
        200,
    )
    .await;
    let bob = register(c, "acme", "bob@example.com").await;
    call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/subscriptions",
        Some(&bob),
        json!({ "plan_id": plan_id, "shipping_address": { "line1": "Weg 2",
                "postal_code": "10115", "city": "Berlin", "country": "DE" } }),
        404,
    )
    .await;
    // Bob can't touch Ada's subscription.
    act(c, &bob, &sub_id, "cancel", 404).await;

    // Canceled is final.
    call(
        c,
        "acme",
        "POST",
        &format!("/api/subscriptions/{sub_id}/actions"),
        a,
        json!({ "action": "cancel" }),
        200,
    )
    .await;
    act(c, &ada, &sub_id, "resume", 409).await;
    make_due(&pool, "acme").await;
    assert_eq!(soat_subscriptions::renew_due(&res).await.unwrap(), 0);

    // Every step is in the event log.
    for kind in [
        "subscription.created",
        "subscription.skipped",
        "subscription.paused",
        "subscription.resumed",
        "subscription.renewed",
        "subscription.canceled",
    ] {
        let e = call(
            c,
            "acme",
            "GET",
            &format!("/api/events?kind={kind}"),
            a,
            Value::Null,
            200,
        )
        .await;
        assert_eq!(e["items"].as_array().unwrap().len(), 1, "{kind}");
    }

    // The admin lists subscriptions and plans, and adds a plan.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/subscriptions")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Coffee beans")
            && html.contains("canceled")
            && html.contains("Every 2 weeks"),
        "{html}"
    );
    let res = c
        .post("/admin/acme/subscription-plans")
        .header("cookie", &cookie)
        .form(
            &json!({ "product_id": coffee, "name": "Monthly", "interval_count": "1",
                       "interval_unit": "month", "discount_percent": "7.5",
                       "shipping_price": "3.90" }),
        )
        .send()
        .await;
    res.assert_status(303);
    let plans = call(
        c,
        "acme",
        "GET",
        &format!("/api/subscription-plans?product_id={coffee}"),
        a,
        Value::Null,
        200,
    )
    .await;
    let monthly = plans["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Monthly")
        .unwrap()
        .clone();
    assert_eq!(monthly["discount_bps"], 750);
    assert_eq!(monthly["shipping_price_minor"], 390);
    let html = c
        .get("/admin/acme/subscriptions")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("7.50%") && html.contains("3.90 EUR") && html.contains("1 month<"),
        "{html}"
    );

    // Another shop sees none of it.
    for path in [
        format!("/api/subscriptions/{sub_id}"),
        format!("/api/subscription-plans/{plan_id}"),
    ] {
        let method = if path.contains("plans") {
            "PATCH"
        } else {
            "GET"
        };
        let body = if method == "PATCH" {
            json!({ "name": "x" })
        } else {
            Value::Null
        };
        call(c, "other", method, &path, a, body, 404).await;
    }
    let none = call(c, "other", "GET", "/api/subscriptions", a, Value::Null, 200).await;
    assert!(none["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn buyers_subscribe_from_the_product_page() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let tea = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "TEA", "title": "Green tea", "price_minor": 900, "currency": "EUR" }),
        201,
    )
    .await;
    let (tea, handle) = (
        tea["id"].as_str().unwrap().to_owned(),
        tea["handle"].as_str().unwrap().to_owned(),
    );
    let loc = call(
        c,
        "acme",
        "POST",
        "/api/locations",
        a,
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/inventory/{tea}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 10, "reason": "received" }),
        200,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        "/api/subscription-plans",
        a,
        json!({ "product_id": tea, "name": "Tea club", "interval_unit": "month",
                "interval_count": 1, "discount_bps": 750 }),
        201,
    )
    .await;
    let page = |path: String, me: Option<String>| async move {
        let req = c.get(&path).header("x-soat-tenant", "acme");
        let req = match me {
            Some(m) => req.header("cookie", &m),
            None => req,
        };
        req.send().await.text()
    };

    let html = page(format!("/products/{handle}"), None).await;
    assert!(html.contains("Tea club · every month · −7.5%"), "{html}");

    // Signed out, subscribing asks to sign in.
    let form = |plan: &str| json!({ "plan_id": plan, "quantity": "2" });
    let plan = call(
        c,
        "acme",
        "GET",
        "/api/subscription-plans",
        a,
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = c
        .post(&format!("/products/{handle}/subscribe"))
        .header("x-soat-tenant", "acme")
        .form(&form(&plan))
        .send()
        .await;
    res.assert_status(303);
    assert!(res.header("location").unwrap().ends_with("/account/login"));

    // Without a saved address, the buyer is told to save one.
    let ada = register(c, "acme", "ada@example.com").await;
    let me = format!("soat_customer={ada}");
    let res = c
        .post(&format!("/products/{handle}/subscribe"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&form(&plan))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.text().contains("Save an address in your account first"));

    // With one, the first order is placed and shown.
    let ada_id = call(
        c,
        "acme",
        "GET",
        "/api/customers?q=ada",
        a,
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ada_id}/addresses"),
        a,
        json!({ "first_name": "Ada", "last_name": "L", "line1": "Hauptstr. 1",
                "postal_code": "10115", "city": "Berlin", "country": "DE" }),
        201,
    )
    .await;
    let res = c
        .post(&format!("/products/{handle}/subscribe"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&form(&plan))
        .send()
        .await;
    res.assert_status(303);
    assert!(res.header("location").unwrap().ends_with("/order"));
    let order = res.cookie("soat_order").unwrap().value().to_owned();
    let html = page("/order".into(), Some(format!("{me}; soat_order={order}"))).await;
    assert!(html.contains("Green tea"), "{html}");

    // The account lists it; the buyer pauses it there.
    let html = page("/account".into(), Some(me.clone())).await;
    assert!(html.contains("/account/subscriptions"), "{html}");
    let html = page("/account/subscriptions".into(), Some(me.clone())).await;
    assert!(
        html.contains("Green tea × 2") && html.contains("Active"),
        "{html}"
    );
    let id = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/subscriptions",
        Some(&ada),
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = c
        .post(&format!("/account/subscriptions/{id}/pause"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .send()
        .await;
    res.assert_status(303);
    let html = page("/account/subscriptions".into(), Some(me.clone())).await;
    assert!(html.contains("Paused") && html.contains("Resume"), "{html}");
    // Someone else's subscription is not found.
    let bob = register(c, "acme", "bob@example.com").await;
    let res = c
        .post(&format!("/account/subscriptions/{id}/cancel"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &format!("soat_customer={bob}"))
        .send()
        .await;
    res.assert_status(404);
}
