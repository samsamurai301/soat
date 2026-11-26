//! Customer access links: activating an email that ordered as a guest,
//! and resetting a forgotten password, by a link sent by email.

mod common;

use serde_json::{json, Value};
use zapline::mail::{Email, Mailer};
use zapline::test::TestClient;

struct Shop {
    client: TestClient,
    admin: String,
}

impl Shop {
    async fn call(&self, shop: &str, method: &str, path: &str, body: Value, status: u16) -> Value {
        let req = match method {
            "POST" => self.client.post(path),
            "PUT" => self.client.put(path),
            _ => self.client.get(path),
        }
        .header("x-soat-tenant", shop)
        .bearer(&self.admin);
        let req = if body.is_null() { req } else { req.json(&body) };
        let res = req.send().await;
        let text = res.text();
        assert_eq!(res.status().as_u16(), status, "{method} {path}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// A storefront call: no staff token.
    async fn shopper(&self, shop: &str, path: &str, body: Value, status: u16) -> Value {
        let res = self
            .client
            .post(path)
            .header("x-soat-tenant", shop)
            .json(&body)
            .send()
            .await;
        let text = res.text();
        assert_eq!(res.status().as_u16(), status, "{path}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    async fn ask(&self, shop: &str, email: &str) {
        let out = self
            .shopper(
                shop,
                "/api/storefront/account/access",
                json!({ "email": email }),
                200,
            )
            .await;
        assert_eq!(out, json!({ "sent": true }));
    }

    async fn sent(&self) -> Vec<Email> {
        let res = self.client.resources().await;
        soat_events::dispatch_resources(&res).await.unwrap();
        soat_events::dispatch_resources(&res).await.unwrap();
        let mailer: Mailer = res.require().unwrap();
        mailer.sent()
    }
}

fn token_in(email: &Email) -> String {
    let text = email.text.clone().unwrap_or_default();
    let at = text.find("token=").expect("a link") + "token=".len();
    text[at..].split_whitespace().next().unwrap().to_owned()
}

#[tokio::test]
async fn a_guest_activates_and_a_customer_resets_by_email() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let s = Shop { client, admin };
    s.call(
        "acme",
        "POST",
        "/api/domains",
        json!({ "domain": "shop.acme.example" }),
        201,
    )
    .await;
    s.call(
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "gus@example.com", "first_name": "Gus" }),
        201,
    )
    .await;

    // Registering an email that ordered as a guest needs a link.
    let res = s
        .shopper(
            "acme",
            "/api/storefront/account/register",
            json!({ "email": "gus@example.com", "password": "a long password" }),
            409,
        )
        .await;
    assert_eq!(res["code"], "activation_required", "{res}");

    // Asking answers the same for anyone; only a customer gets an email,
    // and only from their own shop.
    s.ask("acme", "nobody@example.com").await;
    s.ask("other", "gus@example.com").await;
    s.ask("acme", "Gus@Example.com").await;
    let sent = s.sent().await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    let mail = &sent[0];
    assert_eq!(mail.to, ["gus@example.com"]);
    assert!(mail.subject.contains("Activate your account"), "{mail:?}");
    let text = mail.text.clone().unwrap();
    assert!(text.contains("Hello Gus"));
    assert!(text.contains("https://shop.acme.example/account/access?token="));
    let token = token_in(mail);

    // The token is in no event and not kept in the email log.
    let events = s.call("acme", "GET", "/api/events", Value::Null, 200).await;
    assert!(events.to_string().contains("customer.access_requested"));
    assert!(!events.to_string().contains(&token));
    let log = s
        .call(
            "acme",
            "GET",
            "/api/notifications/messages?template=account_activation",
            Value::Null,
            200,
        )
        .await;
    let id = log["items"][0]["id"].as_str().unwrap().to_owned();
    let kept = s
        .call(
            "acme",
            "GET",
            &format!("/api/notifications/messages/{id}"),
            Value::Null,
            200,
        )
        .await;
    assert!(!kept.to_string().contains(&token), "{kept}");
    let refused = s
        .call(
            "acme",
            "POST",
            &format!("/api/notifications/messages/{id}/resend"),
            Value::Null,
            422,
        )
        .await;
    assert_eq!(refused["code"], "not_resendable");

    // A wrong token does nothing; the right one sets the password once.
    let complete = "/api/storefront/account/access/complete";
    let bad = s
        .shopper(
            "acme",
            complete,
            json!({ "token": "nope", "password": "a long password" }),
            422,
        )
        .await;
    assert_eq!(bad["code"], "invalid_link");
    s.shopper(
        "other",
        complete,
        json!({ "token": token, "password": "a long password" }),
        422,
    )
    .await;
    let session = s
        .shopper(
            "acme",
            complete,
            json!({ "token": token, "password": "a long password" }),
            200,
        )
        .await;
    assert_eq!(session["customer"]["email"], "gus@example.com");
    let first_session = session["token"].as_str().unwrap().to_owned();
    s.shopper(
        "acme",
        complete,
        json!({ "token": token, "password": "another password" }),
        422,
    )
    .await;
    s.shopper(
        "acme",
        "/api/storefront/account/login",
        json!({ "email": "gus@example.com", "password": "a long password" }),
        200,
    )
    .await;

    // Now a reset: a new password, and the old sessions end.
    s.ask("acme", "gus@example.com").await;
    let sent = s.sent().await;
    assert_eq!(sent.len(), 2);
    assert!(sent[1].subject.contains("Reset your password"));
    let token = token_in(&sent[1]);
    s.shopper(
        "acme",
        complete,
        json!({ "token": token, "password": "a newer password" }),
        200,
    )
    .await;
    s.client
        .get("/api/storefront/account")
        .header("x-soat-tenant", "acme")
        .bearer(&first_session)
        .send()
        .await
        .assert_status(401);
    s.shopper(
        "acme",
        "/api/storefront/account/login",
        json!({ "email": "gus@example.com", "password": "a newer password" }),
        200,
    )
    .await;

    // At most three links an hour.
    s.ask("acme", "gus@example.com").await;
    s.ask("acme", "gus@example.com").await;
    assert_eq!(s.sent().await.len(), 3);
}

#[tokio::test]
async fn the_storefront_pages_send_and_use_links() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let s = Shop { client, admin };
    s.call(
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "ida@example.com" }),
        201,
    )
    .await;
    let page = |path: &str| s.client.get(path).header("x-soat-tenant", "acme").send();
    let login = page("/account/login").await.text();
    assert!(login.contains("/account/forgot"));

    // Registering a guest's email offers the link instead.
    let html = s
        .client
        .post("/account/register")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "email": "ida@example.com", "password": "a long password" }))
        .send()
        .await
        .text();
    assert!(html.contains("ordered here before"), "{html}");

    let html = s
        .client
        .post("/account/forgot")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "email": "ida@example.com" }))
        .send()
        .await
        .text();
    assert!(html.contains("a link is on its way"), "{html}");
    let sent = s.sent().await;
    assert_eq!(sent.len(), 1);
    // Without a domain the email gives a code to enter.
    let text = sent[0].text.clone().unwrap();
    assert!(!text.contains("token="), "{text}");
    let code = text
        .split("password page: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    let form = page("/account/access").await.text();
    assert!(form.contains("name=\"token\""));
    let res = s
        .client
        .post("/account/access")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "token": "wrong", "password": "a long password" }))
        .send()
        .await;
    res.assert_status(422);
    let res = s
        .client
        .post("/account/access")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "token": code, "password": "a long password" }))
        .send()
        .await;
    res.assert_status(303);
    assert!(res.cookie("soat_customer").is_some());
}
