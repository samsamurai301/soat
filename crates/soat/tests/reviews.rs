//! I3: product reviews. Signed-in buyers review products, staff publish
//! them, the storefront shows the published ones with their average.
#![cfg(feature = "reviews")]

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

async fn register(c: &TestClient, shop: &str, email: &str, first: &str, last: &str) -> String {
    let s = call(
        c,
        shop,
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": email, "password": "a long password",
                "first_name": first, "last_name": last }),
        201,
    )
    .await;
    s["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn buyers_review_and_staff_publish() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let book = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "BOOK", "title": "Field guide", "price_minor": 900, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let write = format!("/api/storefront/products/{book}/review");
    let shown = format!("/api/storefront/products/{book}/reviews");

    // Only a signed-in customer writes one, for a product of the shop.
    call(c, "acme", "PUT", &write, None, json!({ "rating": 4 }), 401).await;
    let ada = register(c, "acme", "ada@example.com", "Ada", "lovelace").await;
    call(
        c,
        "acme",
        "PUT",
        &write,
        Some(&ada),
        json!({ "rating": 6 }),
        422,
    )
    .await;
    call(
        c,
        "acme",
        "PUT",
        "/api/storefront/products/prod_nope/review",
        Some(&ada),
        json!({ "rating": 4 }),
        404,
    )
    .await;
    let r = call(
        c,
        "acme",
        "PUT",
        &write,
        Some(&ada),
        json!({ "rating": 4, "title": "Handy", "body": "Fits a pocket." }),
        200,
    )
    .await;
    assert_eq!(r["author_name"], "Ada L.");
    assert_eq!(r["status"], "pending");
    assert_eq!(r["verified"], false);
    let id = r["id"].as_str().unwrap().to_owned();

    // Waiting for the shop: buyers see nothing yet.
    let s = call(c, "acme", "GET", &shown, None, Value::Null, 200).await;
    assert_eq!(
        s,
        json!({ "count": 0, "average_tenths": null, "reviews": [] })
    );

    // Staff publish it, with a reply.
    let pending = call(
        c,
        "acme",
        "GET",
        "/api/reviews?status=pending",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(pending["items"][0]["id"], id.as_str());
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/reviews/{id}"),
        a,
        json!({ "status": "gone" }),
        422,
    )
    .await;
    let r = call(
        c,
        "acme",
        "PATCH",
        &format!("/api/reviews/{id}"),
        a,
        json!({ "status": "published", "reply": "Thank you!" }),
        200,
    )
    .await;
    assert_eq!(r["status"], "published");
    let s = call(c, "acme", "GET", &shown, None, Value::Null, 200).await;
    assert_eq!(s["count"], 1);
    assert_eq!(s["average_tenths"], 40);
    assert_eq!(s["reviews"][0]["reply"], "Thank you!");
    assert!(s["reviews"][0].get("customer_id").is_none(), "{s}");

    // A buyer who bought it is verified.
    let bob = register(c, "acme", "bob@example.com", "Bob", "").await;
    let customers = call(
        c,
        "acme",
        "GET",
        "/api/customers?q=bob",
        a,
        Value::Null,
        200,
    )
    .await;
    let bob_id = customers["items"][0]["id"].clone();
    let d = call(
        c,
        "acme",
        "POST",
        "/api/draft-orders",
        a,
        json!({ "currency": "EUR", "customer_id": bob_id, "email": "bob@example.com",
                "billing_address": { "line1": "Hauptstr. 1", "postal_code": "10115",
                                     "city": "Berlin", "country": "DE" },
                "lines": [{ "item_id": book, "quantity": 1 }] }),
        201,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        &format!("/api/draft-orders/{}/complete", d["id"].as_str().unwrap()),
        a,
        json!({ "paid": true }),
        201,
    )
    .await;
    let r = call(
        c,
        "acme",
        "PUT",
        &write,
        Some(&bob),
        json!({ "rating": 5 }),
        200,
    )
    .await;
    assert_eq!(r["verified"], true);
    assert_eq!(r["author_name"], "Bob");
    let bob_review = r["id"].as_str().unwrap().to_owned();
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/reviews/{bob_review}"),
        a,
        json!({ "status": "published" }),
        200,
    )
    .await;
    let s = call(c, "acme", "GET", &shown, None, Value::Null, 200).await;
    assert_eq!(
        (s["count"].clone(), s["average_tenths"].clone()),
        (json!(2), json!(45))
    );

    // Writing again replaces the review and sends it back to moderation.
    let again = call(
        c,
        "acme",
        "PUT",
        &write,
        Some(&ada),
        json!({ "rating": 2 }),
        200,
    )
    .await;
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["status"], "pending");
    let s = call(c, "acme", "GET", &shown, None, Value::Null, 200).await;
    assert_eq!(
        (s["count"].clone(), s["average_tenths"].clone()),
        (json!(1), json!(50))
    );

    // Another shop sees and reaches none of it.
    let theirs = call(c, "other", "GET", "/api/reviews", a, Value::Null, 200).await;
    assert_eq!(theirs["items"], json!([]));
    call(
        c,
        "other",
        "GET",
        &format!("/api/reviews/{id}"),
        a,
        Value::Null,
        404,
    )
    .await;
    call(
        c,
        "other",
        "PATCH",
        &format!("/api/reviews/{id}"),
        a,
        json!({ "status": "published" }),
        404,
    )
    .await;
    call(
        c,
        "other",
        "DELETE",
        &format!("/api/reviews/{id}"),
        a,
        Value::Null,
        404,
    )
    .await;
    let s = call(c, "other", "GET", &shown, None, Value::Null, 200).await;
    assert_eq!(s["count"], 0);

    // The product page shows the published review and its rating, and a
    // signed-in buyer sends one with the form.
    let page = |cookie: Option<String>| {
        let req = c
            .get("/products/field-guide")
            .header("x-soat-tenant", "acme");
        let req = match cookie {
            Some(v) => req.header("cookie", &v),
            None => req,
        };
        async move { req.send().await.text() }
    };
    let html = page(None).await;
    assert!(html.contains("id=\"reviews\""), "{html}");
    assert!(html.contains("Bob") && html.contains("5.0 · 1"), "{html}");
    assert!(html.contains("\"aggregateRating\""), "{html}");
    assert!(html.contains("Sign in to write a review"));
    let me = format!("soat_customer={ada}");
    assert!(page(Some(me.clone())).await.contains("Send review"));
    let res = c
        .post("/products/field-guide/review")
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&json!({ "rating": "3", "title": "Fine", "body": "Ok." }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(
        res.header("location"),
        Some("/products/field-guide?reviewed=1#reviews")
    );
    let mine = call(
        c,
        "acme",
        "GET",
        &format!("/api/reviews/{id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        (mine["rating"].clone(), mine["title"].clone()),
        (json!(3), json!("Fine"))
    );
    let res = c
        .post("/products/field-guide/review")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "rating": "3" }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/account/login"));

    // The admin lists waiting reviews and publishes them.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/reviews")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Fine") && html.contains("Publish"), "{html}");
    let res = c
        .post(&format!("/admin/acme/reviews/{id}/publish"))
        .header("cookie", &cookie)
        .form(&json!({ "back": "pending" }))
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/acme/reviews"),
        "{}",
        res.text()
    );
    let r = call(
        c,
        "acme",
        "GET",
        &format!("/api/reviews/{id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(r["status"], "published");

    // Deleted, it is gone; the event log says what happened.
    call(
        c,
        "acme",
        "DELETE",
        &format!("/api/reviews/{bob_review}"),
        a,
        Value::Null,
        204,
    )
    .await;
    call(
        c,
        "acme",
        "GET",
        &format!("/api/reviews/{bob_review}"),
        a,
        Value::Null,
        404,
    )
    .await;
    let events = call(c, "acme", "GET", "/api/events", a, Value::Null, 200).await;
    let text = events.to_string();
    for kind in ["review.created", "review.updated", "review.deleted"] {
        assert!(text.contains(kind), "{kind}");
    }
}
