//! I5: loyalty points and referrals. Customers earn points on paid
//! orders, turn them into store credit and refer friends.
#![cfg(feature = "loyalty")]

mod common;

use serde_json::{json, Value};
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
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
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

async fn register(c: &TestClient, email: &str) -> String {
    let s = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": email, "password": "a long password" }),
        201,
    )
    .await;
    s["token"].as_str().unwrap().to_owned()
}

async fn customer_id(c: &TestClient, admin: &str, search: &str) -> String {
    let found = call(
        c,
        "acme",
        "GET",
        &format!("/api/customers?q={search}"),
        Some(admin),
        Value::Null,
        200,
    )
    .await;
    found["items"][0]["id"].as_str().unwrap().to_owned()
}

/// A paid phone order of `quantity` books for `customer`; its id.
async fn paid_order(
    c: &TestClient,
    admin: &str,
    customer: &str,
    item: &str,
    quantity: i64,
) -> String {
    let d = call(
        c,
        "acme",
        "POST",
        "/api/draft-orders",
        Some(admin),
        json!({ "currency": "EUR", "customer_id": customer, "email": "buyer@example.com",
                "billing_address": { "line1": "Hauptstr. 1", "postal_code": "10115",
                                     "city": "Berlin", "country": "DE" },
                "lines": [{ "item_id": item, "quantity": quantity }] }),
        201,
    )
    .await;
    let done = call(
        c,
        "acme",
        "POST",
        &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
        Some(admin),
        json!({ "paid": true }),
        201,
    )
    .await;
    done["order"]["id"].as_str().unwrap().to_owned()
}

/// A customer's balance once events are handled, waiting for `want`.
async fn balance_of(c: &TestClient, token: &str, want: i64) -> i64 {
    let res = c.resources().await;
    let mut got = -1;
    for _ in 0..50 {
        soat_events::dispatch_resources(&res).await.unwrap();
        let m = call(
            c,
            "acme",
            "GET",
            "/api/storefront/account/loyalty",
            Some(token),
            Value::Null,
            200,
        )
        .await;
        got = m["balance"].as_i64().unwrap();
        if got == want {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    got
}

#[tokio::test]
async fn customers_earn_spend_and_refer() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());

    // Off by default; the owner turns it on.
    let p = call(c, "acme", "GET", "/api/loyalty", a, Value::Null, 200).await;
    assert_eq!(p["enabled"], false);
    assert_eq!(p["points_per_credit"], 100);
    call(
        c,
        "acme",
        "PUT",
        "/api/loyalty",
        a,
        json!({ "enabled": true, "points_per_credit": 0 }),
        422,
    )
    .await;
    let ada = register(c, "ada@example.com").await;
    let off = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/loyalty/redeem",
        Some(&ada),
        json!({ "points": 100, "currency": "EUR" }),
        409,
    )
    .await;
    assert_eq!(off["code"], "loyalty_off");
    let p = call(
        c,
        "acme",
        "PUT",
        "/api/loyalty",
        a,
        json!({ "enabled": true, "points_per_unit": 2 }),
        200,
    )
    .await;
    assert_eq!(
        (
            p["enabled"].clone(),
            p["points_per_unit"].clone(),
            p["referrer_points"].clone()
        ),
        (json!(true), json!(2), json!(500))
    );
    let theirs = call(c, "other", "GET", "/api/loyalty", a, Value::Null, 200).await;
    assert_eq!(theirs["enabled"], false);

    // Ada's code; Bob enters it before his first order. Unknown, own and
    // second codes are refused.
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/loyalty",
        Some(&ada),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine["balance"], 0);
    assert_eq!(mine["can_enter_referral"], true);
    let code = mine["referral_code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 8);
    let refer = "/api/storefront/account/loyalty/referral";
    let r = call(
        c,
        "acme",
        "POST",
        refer,
        Some(&ada),
        json!({ "code": code }),
        422,
    )
    .await;
    assert_eq!(r["code"], "invalid_referral");
    let bob = register(c, "bob@example.com").await;
    call(
        c,
        "acme",
        "POST",
        refer,
        Some(&bob),
        json!({ "code": "NOPE1234" }),
        422,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        refer,
        Some(&bob),
        json!({ "code": code.to_lowercase() }),
        204,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        refer,
        Some(&bob),
        json!({ "code": code }),
        422,
    )
    .await;

    // Bob's first paid order: 2 points a euro of goods, his welcome
    // points, and Ada's for referring him.
    let book = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "BOOK", "title": "Field guide", "price_minor": 2050, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob_id = customer_id(c, &admin, "bob").await;
    let ada_id = customer_id(c, &admin, "ada").await;
    paid_order(c, &admin, &bob_id, &book, 2).await;
    assert_eq!(balance_of(c, &bob, 82 + 250).await, 332);
    assert_eq!(balance_of(c, &ada, 500).await, 500);
    // A second order earns, but no second referral.
    let second = paid_order(c, &admin, &bob_id, &book, 1).await;
    assert_eq!(balance_of(c, &bob, 332 + 41).await, 373);
    assert_eq!(balance_of(c, &ada, 500).await, 500);
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/loyalty",
        Some(&bob),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine["referred"], true);
    assert_eq!(mine["can_enter_referral"], false);
    let kinds: Vec<&str> = mine["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["earned", "welcome", "earned"]);

    // Refunded in full, the second order's points go again.
    let total = call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{second}"),
        a,
        Value::Null,
        200,
    )
    .await["total_minor"]
        .clone();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/orders/{second}/refunds"),
        a,
        json!({ "amount_minor": total, "reason": "Changed mind" }),
        201,
    )
    .await;
    assert_eq!(balance_of(c, &bob, 332).await, 332);

    // Bob turns points into store credit, spent at checkout.
    let redeem = "/api/storefront/account/loyalty/redeem";
    let r = call(
        c,
        "acme",
        "POST",
        redeem,
        Some(&bob),
        json!({ "points": 50, "currency": "EUR" }),
        422,
    )
    .await;
    assert_eq!(r["code"], "invalid_redeem");
    let r = call(
        c,
        "acme",
        "POST",
        redeem,
        Some(&bob),
        json!({ "points": 400, "currency": "EUR" }),
        422,
    )
    .await;
    assert_eq!(r["code"], "insufficient_points");
    let r = call(
        c,
        "acme",
        "POST",
        redeem,
        Some(&bob),
        json!({ "points": 300, "currency": "EUR" }),
        200,
    )
    .await;
    assert_eq!(r["balance"], 32);
    assert_eq!(r["store_credit"]["balance_minor"], 300);
    let credit = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/store-credit",
        Some(&bob),
        Value::Null,
        200,
    )
    .await;
    assert!(credit.to_string().contains("300"), "{credit}");

    // Staff see and adjust points; never below zero.
    let pts = format!("/api/loyalty/customers/{ada_id}");
    let s = call(c, "acme", "GET", &pts, a, Value::Null, 200).await;
    assert_eq!(s["balance"], 500);
    assert_eq!(s["entries"][0]["kind"], "referral");
    let adjust = format!("{pts}/adjustments");
    let r = call(
        c,
        "acme",
        "POST",
        &adjust,
        a,
        json!({ "points": -600 }),
        422,
    )
    .await;
    assert_eq!(r["code"], "insufficient_points");
    let s = call(
        c,
        "acme",
        "POST",
        &adjust,
        a,
        json!({ "points": 25, "note": "Thank you" }),
        200,
    )
    .await;
    assert_eq!(s["balance"], 525);
    assert_eq!(s["entries"][0]["note"], "Thank you");
    let s = call(
        c,
        "acme",
        "GET",
        &format!("/api/loyalty/customers/{bob_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s["referred_by"], ada_id.as_str());

    // Another shop sees and reaches none of it.
    call(c, "other", "GET", &pts, a, Value::Null, 404).await;
    call(c, "other", "POST", &adjust, a, json!({ "points": 5 }), 404).await;

    // The storefront: the points page, the referral link, and a friend
    // who registers with it is referred.
    let me = format!("soat_customer={ada}");
    let html = c
        .get("/account/loyalty")
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .send()
        .await
        .text();
    assert!(html.contains("525") && html.contains(&code), "{html}");
    assert!(html.contains("Turn into store credit"), "{html}");
    let html = c
        .get(&format!("/account/register?ref={code}"))
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .text();
    assert!(html.contains(&format!("value=\"{code}\"")), "{html}");
    let res = c
        .post("/account/register")
        .header("x-soat-tenant", "acme")
        .form(
            &json!({ "email": "cy@example.com", "password": "a long password",
                       "referral_code": code }),
        )
        .send()
        .await;
    res.assert_status(303);
    let cy = customer_id(c, &admin, "cy").await;
    let s = call(
        c,
        "acme",
        "GET",
        &format!("/api/loyalty/customers/{cy}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s["referred_by"], ada_id.as_str());
    let res = c
        .post("/account/loyalty/redeem")
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&json!({ "points": "200" }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(
        res.header("location"),
        Some("/account/loyalty?done=redeemed")
    );
    let left = call(c, "acme", "GET", &pts, a, Value::Null, 200).await;
    assert_eq!(left["balance"], 325);

    // The admin: the program on the gift cards page, points on the
    // customer's page.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/gift-cards")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Loyalty points"), "{html}");
    let html = c
        .get(&format!("/admin/acme/customers/{ada_id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Loyalty points") && html.contains("325"),
        "{html}"
    );
    let res = c
        .post(&format!("/admin/acme/customers/{ada_id}/points"))
        .header("cookie", &cookie)
        .form(&json!({ "points": "-25", "note": "Fix" }))
        .send()
        .await;
    res.assert_status(303);
    let res = c
        .post("/admin/acme/loyalty")
        .header("cookie", &cookie)
        .form(
            &json!({ "points_per_unit": "1", "points_per_credit": "100", "min_redeem": "100",
                       "referrer_points": "500", "referee_points": "250" }),
        )
        .send()
        .await;
    res.assert_status(303);
    let p = call(c, "acme", "GET", "/api/loyalty", a, Value::Null, 200).await;
    assert_eq!(p["enabled"], false);
    assert_eq!(
        call(c, "acme", "GET", &pts, a, Value::Null, 200).await["balance"],
        300
    );

    // The event log says what happened.
    let events = call(
        c,
        "acme",
        "GET",
        "/api/events?limit=200",
        a,
        Value::Null,
        200,
    )
    .await;
    let text = events.to_string();
    for kind in [
        "loyalty.updated",
        "loyalty.referral_entered",
        "loyalty.points_changed",
    ] {
        assert!(text.contains(kind), "{kind}");
    }
}
