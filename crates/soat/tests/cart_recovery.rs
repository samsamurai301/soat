//! I2: abandoned cart emails. A shop turns them on; a cart left with an
//! email gets one email with a link that brings the cart back.

mod common;

use serde_json::{json, Value};
use zapline::db::sqlx::{self, PgPool};
use zapline::mail::{Email, Mailer};
use zapline::test::TestClient;

async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "PATCH" => client.patch(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = headers.iter().fold(req, |r, (k, v)| r.header(*k, *v));
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// The emails sent once events are handled, waiting for `n` of them.
async fn sent(client: &TestClient, n: usize) -> Vec<Email> {
    let res = client.resources().await;
    let mailer: Mailer = res.require().unwrap();
    for _ in 0..50 {
        soat_events::dispatch_resources(&res).await.unwrap();
        if mailer.sent().len() >= n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    mailer.sent()
}

/// Makes the shop's carts look untouched for `minutes`.
async fn age(pool: &PgPool, shop: &str, minutes: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', $1, true)")
        .bind(shop)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE carts SET updated_at = now() - make_interval(mins => $1)")
        .bind(minutes)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// A storefront cart in `shop` with `email`, holding `item` unless None.
async fn cart(
    c: &TestClient,
    shop: &str,
    email: Option<&str>,
    item: Option<&str>,
) -> (String, String) {
    let body = match email {
        Some(e) => json!({ "currency": "EUR", "email": e }),
        None => json!({ "currency": "EUR" }),
    };
    let made = call(c, shop, "POST", "/api/storefront/carts", &[], body, 201).await;
    let id = made["cart"]["id"].as_str().unwrap().to_owned();
    let token = made["token"].as_str().unwrap().to_owned();
    if let Some(item) = item {
        call(
            c,
            shop,
            "POST",
            &format!("/api/storefront/carts/{id}/lines"),
            &[("x-soat-cart-token", &token)],
            json!({ "item_id": item, "quantity": 2 }),
            200,
        )
        .await;
    }
    (id, token)
}

#[tokio::test]
async fn a_left_cart_gets_one_email_that_brings_it_back() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let bearer = format!("Bearer {admin}");
    let staff = [("authorization", bearer.as_str())];
    let pool: PgPool = c.resources().await.require().unwrap();

    // Off by default; the owner turns it on.
    let s = call(
        c,
        "acme",
        "GET",
        "/api/cart-recovery",
        &staff,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s, json!({ "enabled": false, "delay_minutes": 60 }));
    call(
        c,
        "acme",
        "PUT",
        "/api/cart-recovery",
        &staff,
        json!({ "enabled": true, "delay_minutes": 5 }),
        422,
    )
    .await;
    let s = call(
        c,
        "acme",
        "PUT",
        "/api/cart-recovery",
        &staff,
        json!({ "enabled": true, "delay_minutes": 30 }),
        200,
    )
    .await;
    assert_eq!(s, json!({ "enabled": true, "delay_minutes": 30 }));
    let theirs = call(
        c,
        "other",
        "GET",
        "/api/cart-recovery",
        &staff,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(theirs["enabled"], false);
    call(
        c,
        "acme",
        "POST",
        "/api/domains",
        &staff,
        json!({ "domain": "shop.acme.example" }),
        201,
    )
    .await;

    // Carts: one left with an email, one without an email, one empty,
    // and one in a shop that has recovery off.
    let mug = call(
        c,
        "acme",
        "POST",
        "/api/products",
        &staff,
        json!({ "sku": "MUG", "title": "Mug", "price_minor": 1200, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (left, old_token) = cart(c, "acme", Some("ada@example.com"), Some(&mug)).await;
    cart(c, "acme", None, Some(&mug)).await;
    cart(c, "acme", Some("bob@example.com"), None).await;
    cart(c, "other", Some("cy@example.com"), None).await;

    // Too fresh: nothing yet. Left long enough: one email, once.
    assert_eq!(soat_cart::recovery::scan(&pool).await.unwrap(), 0);
    age(&pool, "acme", 45).await;
    age(&pool, "other", 45).await;
    assert_eq!(soat_cart::recovery::scan(&pool).await.unwrap(), 1);
    assert_eq!(soat_cart::recovery::scan(&pool).await.unwrap(), 0);
    let mails: Vec<Email> = sent(c, 1)
        .await
        .into_iter()
        .filter(|m| m.subject.contains("left something"))
        .collect();
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert_eq!(mails[0].to, ["ada@example.com"]);
    let text = mails[0].text.clone().unwrap();
    assert!(text.contains("2 x Mug"), "{text}");
    let at = text
        .find("https://shop.acme.example/cart/recover?token=")
        .expect("a link")
        + "https://shop.acme.example/cart/recover?token=".len();
    let link = text[at..].split_whitespace().next().unwrap().to_owned();

    // The link's token is in no event and not kept in the email log.
    let events = call(c, "acme", "GET", "/api/events", &staff, Value::Null, 200).await;
    assert!(events.to_string().contains("cart.abandoned"));
    assert!(!events.to_string().contains(&link));
    let log = call(
        c,
        "acme",
        "GET",
        "/api/notifications/messages?template=cart_recovery",
        &staff,
        Value::Null,
        200,
    )
    .await;
    let id = log["items"][0]["id"].as_str().unwrap().to_owned();
    let kept = call(
        c,
        "acme",
        "GET",
        &format!("/api/notifications/messages/{id}"),
        &staff,
        Value::Null,
        200,
    )
    .await;
    assert!(!kept.to_string().contains(&link), "{kept}");
    call(
        c,
        "acme",
        "POST",
        &format!("/api/notifications/messages/{id}/resend"),
        &staff,
        Value::Null,
        422,
    )
    .await;

    // Another shop cannot open it; the shop hands the cart back under a
    // new token, and the old one stops working.
    let recover = json!({ "token": link });
    call(
        c,
        "other",
        "POST",
        "/api/storefront/carts/recover",
        &[],
        recover.clone(),
        404,
    )
    .await;
    let back = call(
        c,
        "acme",
        "POST",
        "/api/storefront/carts/recover",
        &[],
        recover.clone(),
        200,
    )
    .await;
    assert_eq!(back["cart"]["id"], left.as_str());
    assert_eq!(back["cart"]["lines"][0]["quantity"], 2);
    let new_token = back["token"].as_str().unwrap().to_owned();
    let path = format!("/api/storefront/carts/{left}");
    call(
        c,
        "acme",
        "GET",
        &path,
        &[("x-soat-cart-token", &old_token)],
        Value::Null,
        404,
    )
    .await;
    call(
        c,
        "acme",
        "GET",
        &path,
        &[("x-soat-cart-token", &new_token)],
        Value::Null,
        200,
    )
    .await;

    // In the browser, the link puts the cart in a cookie and shows it.
    let res = c
        .get(&format!("/cart/recover?token={link}"))
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/cart"));
    let cookie = res.cookie("soat_cart").unwrap().value().to_owned();
    assert!(cookie.starts_with(&format!("{left}.")), "{cookie}");
    let res = c
        .get("/cart/recover?token=nope")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(303);
    assert!(res.cookie("soat_cart").is_none());

    // Touched again, the cart gets no second email.
    age(&pool, "acme", 45).await;
    assert_eq!(soat_cart::recovery::scan(&pool).await.unwrap(), 0);

    // The admin's email settings show and change it.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/settings/email")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Abandoned carts") && html.contains("value=\"30\""),
        "{html}"
    );
    let res = c
        .post("/admin/acme/settings/email/cart-recovery")
        .header("cookie", &cookie)
        .form(&json!({ "delay_minutes": "120" }))
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 303, "{}", res.text());
    let s = call(
        c,
        "acme",
        "GET",
        "/api/cart-recovery",
        &staff,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s, json!({ "enabled": false, "delay_minutes": 120 }));
}
