//! Gift cards sold as products: paying the order issues one card per unit
//! and emails each code to the buyer, once.

mod common;

use serde_json::{json, Value};
use zapline::mail::{Email, Mailer};
use zapline::test::TestClient;

#[allow(clippy::too_many_arguments)]
async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    headers: &[(&str, &str)],
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
    let req = headers.iter().fold(req, |r, (k, v)| r.header(*k, *v));
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// The emails sent once events are handled, waiting for `n` of them (the
/// app's own dispatcher may hold a delivery while ours skips it).
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

fn code_in(email: &Email) -> String {
    let text = email.text.clone().unwrap_or_default();
    let at = text.find("Code: ").expect("a code") + "Code: ".len();
    text[at..].split_whitespace().next().unwrap().to_owned()
}

#[tokio::test]
async fn a_bought_gift_card_is_issued_and_emailed() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let staff = |method: &'static str, path: String, body: Value, status: u16| {
        let admin = &admin;
        async move { call(c, "acme", method, &path, Some(admin), &[], body, status).await }
    };
    let invoice = staff(
        "POST",
        "/api/payment-methods".into(),
        json!({ "provider": "manual", "name": "Invoice" }),
        201,
    )
    .await["id"]
        .clone();
    // A 50 EUR gift card: tagged, and nothing to ship.
    let card = staff(
        "POST",
        "/api/products".into(),
        json!({ "sku": "GIFT-50", "title": "Gift card 50", "price_minor": 5000,
                "currency": "EUR", "requires_shipping": false, "tags": ["gift-card"] }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Two of them in one order.
    let cart = call(
        c,
        "acme",
        "POST",
        "/api/storefront/carts",
        None,
        &[],
        json!({ "currency": "EUR" }),
        201,
    )
    .await;
    let id = cart["cart"]["id"].as_str().unwrap().to_owned();
    let token = cart["token"].as_str().unwrap().to_owned();
    let mine = [("x-soat-cart-token", token.as_str())];
    call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/carts/{id}/lines"),
        None,
        &mine,
        json!({ "item_id": card, "quantity": 2 }),
        200,
    )
    .await;
    let path = format!("/api/storefront/carts/{id}/checkout");
    let v = call(
        c,
        "acme",
        "PATCH",
        &path,
        None,
        &mine,
        json!({ "email": "ada@example.com",
                "billing_address": { "line1": "Hauptstr. 1", "postal_code": "10115",
                                     "city": "Berlin", "country": "DE" } }),
        200,
    )
    .await;
    assert_eq!(v["ready"], true, "{v}");
    let done = call(
        c,
        "acme",
        "POST",
        &format!("{path}/complete"),
        None,
        &mine,
        json!({ "payment_method_id": invoice }),
        201,
    )
    .await;
    let order = done["order"]["id"].as_str().unwrap().to_owned();

    // Unpaid: nothing is issued.
    let before = sent(c, 1).await.len();
    let list = staff("GET", "/api/gift-cards".into(), Value::Null, 200).await;
    assert_eq!(list["items"], json!([]), "{list}");

    // Paid: two cards of 50.00, each code emailed to the buyer.
    let pay = done["payment"]["id"].as_str().unwrap().to_owned();
    staff(
        "POST",
        format!("/api/payments/{pay}/mark-paid"),
        Value::Null,
        200,
    )
    .await;
    let mails: Vec<Email> = sent(c, before + 2)
        .await
        .into_iter()
        .filter(|m| m.subject.contains("gift card"))
        .collect();
    assert_eq!(mails.len(), 2, "{mails:?}");
    assert_eq!(mails[0].to, ["ada@example.com"]);
    assert!(mails[0].subject.contains("50.00"), "{:?}", mails[0]);
    let codes: Vec<String> = mails.iter().map(code_in).collect();
    assert_ne!(codes[0], codes[1]);
    let list = staff("GET", "/api/gift-cards".into(), Value::Null, 200).await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{list}");
    assert!(items.iter().all(|i| i["order_id"] == order.as_str()));
    assert!(items.iter().all(|i| i["balance_minor"] == 5000));

    // The buyer spends a code; another shop does not know it.
    let b = call(
        c,
        "acme",
        "POST",
        "/api/storefront/gift-cards/balance",
        None,
        &[],
        json!({ "code": codes[0] }),
        200,
    )
    .await;
    assert_eq!(b["balance_minor"], 5000);
    call(
        c,
        "other",
        "POST",
        "/api/storefront/gift-cards/balance",
        None,
        &[],
        json!({ "code": codes[0] }),
        404,
    )
    .await;

    // The codes are in no event and not kept in the email log, which
    // cannot resend them.
    let events = staff("GET", "/api/events".into(), Value::Null, 200).await;
    let text = events.to_string();
    assert!(text.contains("gift_card.issued"), "{text}");
    assert!(codes.iter().all(|code| !text.contains(code.as_str())));
    let log = staff(
        "GET",
        "/api/notifications/messages?template=gift_card_issued".into(),
        Value::Null,
        200,
    )
    .await;
    let logged = log["items"].as_array().unwrap();
    assert_eq!(logged.len(), 2, "{log}");
    let first = logged[0]["id"].as_str().unwrap().to_owned();
    let kept = staff(
        "GET",
        format!("/api/notifications/messages/{first}"),
        Value::Null,
        200,
    )
    .await;
    assert!(
        codes
            .iter()
            .all(|code| !kept.to_string().contains(code.as_str())),
        "{kept}"
    );
    let refused = staff(
        "POST",
        format!("/api/notifications/messages/{first}/resend"),
        Value::Null,
        422,
    )
    .await;
    assert_eq!(refused["code"], "not_resendable");

    // Handling the events again issues and sends nothing more.
    let now = sent(c, 0).await.len();
    let list = staff("GET", "/api/gift-cards".into(), Value::Null, 200).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(sent(c, 0).await.len(), now);
}
