//! I6: email campaigns. Staff write a campaign, test it and send it to
//! the customers who agreed to news; every email can unsubscribe in one
//! click.
#![cfg(feature = "marketing")]

mod common;

use serde_json::{json, Value};
use zapline::mail::{Email, Mailer};
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

/// The unsubscribe token in a campaign email.
fn token(m: &Email) -> String {
    let text = m.text.clone().unwrap();
    let key = "https://shop.acme.example/unsubscribe?token=";
    let at = text.find(key).expect("a link") + key.len();
    text[at..].split_whitespace().next().unwrap().to_owned()
}

#[tokio::test]
async fn campaigns_reach_who_agreed_and_let_them_leave() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let res = c.resources().await;
    let mailer: Mailer = res.require().unwrap();
    let mut ids = Vec::new();
    for (shop, email, first, news, tags) in [
        ("acme", "ada@example.com", "Ada", true, json!(["vip"])),
        ("acme", "bob@example.com", "Bob", true, json!([])),
        ("acme", "cy@example.com", "Cy", false, json!([])),
        ("other", "dan@example.com", "Dan", true, json!([])),
    ] {
        let made = call(
            c,
            shop,
            "POST",
            "/api/customers",
            a,
            json!({ "email": email, "first_name": first, "accepts_marketing": news, "tags": tags }),
            201,
        )
        .await;
        ids.push(made["id"].as_str().unwrap().to_owned());
    }

    // Written as a draft; a broken template is refused.
    call(
        c,
        "acme",
        "POST",
        "/api/campaigns",
        a,
        json!({ "name": "Winter", "subject": "Hi", "text_body": "{{#if x }}" }),
        422,
    )
    .await;
    let made = call(
        c,
        "acme",
        "POST",
        "/api/campaigns",
        a,
        json!({ "name": "Winter", "subject": "Hello {{ first_name }}",
                "text_body": "Our winter sale starts, {{ first_name }}!" }),
        201,
    )
    .await;
    assert_eq!(
        (made["status"].clone(), made["recipients"].clone()),
        (json!("draft"), json!(2))
    );
    let id = made["id"].as_str().unwrap().to_owned();
    let path = format!("/api/campaigns/{id}");
    let vip = call(
        c,
        "acme",
        "PATCH",
        &path,
        a,
        json!({ "audience_tag": "vip" }),
        200,
    )
    .await;
    assert_eq!(vip["recipients"], 1);
    let all = call(
        c,
        "acme",
        "PATCH",
        &path,
        a,
        json!({ "audience_tag": "" }),
        200,
    )
    .await;
    assert_eq!(all["recipients"], 2);

    // A test goes to whoever asks; sending needs a domain for the link.
    call(
        c,
        "acme",
        "POST",
        &format!("{path}/test"),
        a,
        json!({ "email": "me@example.com" }),
        204,
    )
    .await;
    let test = mailer
        .sent()
        .into_iter()
        .find(|m| m.to == ["me@example.com"])
        .unwrap();
    assert_eq!(test.subject, "[Test] Hello Ada");
    let r = call(
        c,
        "acme",
        "POST",
        &format!("{path}/send"),
        a,
        Value::Null,
        422,
    )
    .await;
    assert_eq!(r["code"], "no_domain");
    call(
        c,
        "acme",
        "POST",
        "/api/domains",
        a,
        json!({ "domain": "shop.acme.example" }),
        201,
    )
    .await;
    let s = call(
        c,
        "acme",
        "POST",
        &format!("{path}/send"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        (s["status"].clone(), s["recipients"].clone()),
        (json!("sending"), json!(2))
    );
    let r = call(
        c,
        "acme",
        "PATCH",
        &path,
        a,
        json!({ "name": "Later" }),
        409,
    )
    .await;
    assert_eq!(r["code"], "campaign_not_draft");
    call(c, "acme", "DELETE", &path, a, Value::Null, 409).await;

    // In the background: one email each, with a link to unsubscribe.
    assert_eq!(soat_marketing::send_due(&res).await.unwrap(), 2);
    assert_eq!(soat_marketing::send_due(&res).await.unwrap(), 0);
    let mails: Vec<Email> = mailer
        .sent()
        .into_iter()
        .filter(|m| m.subject.starts_with("Hello"))
        .collect();
    let mut to: Vec<String> = mails.iter().map(|m| m.to[0].clone()).collect();
    to.sort();
    assert_eq!(to, ["ada@example.com", "bob@example.com"]);
    let ada_mail = mails.iter().find(|m| m.to == ["ada@example.com"]).unwrap();
    assert_eq!(ada_mail.subject, "Hello Ada");
    assert!(ada_mail.text.as_deref().unwrap().contains("starts, Ada!"));
    let mime = String::from_utf8(ada_mail.to_mime("x@example.com").unwrap()).unwrap();
    assert!(mime.contains("List-Unsubscribe-Post"), "{mime}");
    let ada_token = token(ada_mail);
    let bob_token = token(mails.iter().find(|m| m.to == ["bob@example.com"]).unwrap());
    let done = call(c, "acme", "GET", &path, a, Value::Null, 200).await;
    assert_eq!(
        (
            done["status"].clone(),
            done["sent"].clone(),
            done["failed"].clone()
        ),
        (json!("sent"), json!(2), json!(0))
    );
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
    assert!(text.contains("campaign.sent") && text.contains("campaign.sending"));
    assert!(!text.contains(&ada_token));

    // Another shop sees and reaches none of it.
    let theirs = call(c, "other", "GET", "/api/campaigns", a, Value::Null, 200).await;
    assert_eq!(theirs["items"], json!([]));
    call(c, "other", "GET", &path, a, Value::Null, 404).await;
    call(
        c,
        "other",
        "POST",
        "/api/storefront/unsubscribe",
        None,
        json!({ "token": bob_token }),
        404,
    )
    .await;

    // Opening the link only asks; the button or a mail app's one-click
    // post unsubscribes, again and again.
    let page = c
        .get(&format!("/unsubscribe?token={ada_token}"))
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    page.assert_status(200);
    assert!(page.text().contains(&ada_token));
    let ada = format!("/api/customers/{}", ids[0]);
    assert_eq!(
        call(c, "acme", "GET", &ada, a, Value::Null, 200).await["accepts_marketing"],
        true
    );
    for _ in 0..2 {
        let res = c
            .post(&format!("/unsubscribe?token={ada_token}"))
            .header("x-soat-tenant", "acme")
            .form(&json!({ "List-Unsubscribe": "One-Click" }))
            .send()
            .await;
        res.assert_status(200);
    }
    assert_eq!(
        call(c, "acme", "GET", &ada, a, Value::Null, 200).await["accepts_marketing"],
        false
    );
    let res = c
        .post("/unsubscribe")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "token": "nope" }))
        .send()
        .await;
    res.assert_status(404);
    call(
        c,
        "acme",
        "POST",
        "/api/storefront/unsubscribe",
        None,
        json!({ "token": bob_token }),
        204,
    )
    .await;
    let done = call(c, "acme", "GET", &path, a, Value::Null, 200).await;
    assert_eq!(done["unsubscribed"], 2);

    // Nobody left who wants news.
    let next = call(
        c,
        "acme",
        "POST",
        "/api/campaigns",
        a,
        json!({ "name": "Spring", "subject": "Spring", "text_body": "Hi" }),
        201,
    )
    .await;
    let r = call(
        c,
        "acme",
        "POST",
        &format!("/api/campaigns/{}/send", next["id"].as_str().unwrap()),
        a,
        Value::Null,
        422,
    )
    .await;
    assert_eq!(r["code"], "no_recipients");

    // The admin lists campaigns and shows how one went.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/campaigns")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Winter") && html.contains("Spring"), "{html}");
    let html = c
        .get(&format!("/admin/acme/campaigns/{id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("unsubscribed") && html.contains("Our winter sale"),
        "{html}"
    );
    let res = c
        .post("/admin/acme/campaigns")
        .header("cookie", &cookie)
        .form(&json!({ "name": "Summer", "subject": "Sun", "text_body": "Hello" }))
        .send()
        .await;
    res.assert_status(303);
}
