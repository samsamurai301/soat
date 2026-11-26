//! I4: wishlists. Signed-in buyers save products for later; staff see
//! which products are saved most.
#![cfg(feature = "wishlists")]

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

async fn product(c: &TestClient, admin: &str, sku: &str, title: &str) -> String {
    call(
        c,
        "acme",
        "POST",
        "/api/products",
        Some(admin),
        json!({ "sku": sku, "title": title, "price_minor": 1500, "currency": "EUR" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn buyers_save_products_for_later() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let mug = product(c, &admin, "MUG", "Mug").await;
    let tee = product(c, &admin, "TEE", "Tee").await;
    let ada = register(c, "acme", "ada@example.com").await;
    let bob = register(c, "acme", "bob@example.com").await;
    let list = "/api/storefront/account/wishlist";
    let item = |p: &str| format!("{list}/{p}");

    // Only signed in, and only the shop's own products.
    call(c, "acme", "GET", list, None, Value::Null, 401).await;
    call(c, "acme", "PUT", &item(&mug), None, Value::Null, 401).await;
    call(
        c,
        "acme",
        "PUT",
        &item("prod_nope"),
        Some(&ada),
        Value::Null,
        404,
    )
    .await;
    let theirs = register(c, "other", "cy@example.com").await;
    call(
        c,
        "other",
        "PUT",
        &item(&mug),
        Some(&theirs),
        Value::Null,
        404,
    )
    .await;

    // Saving twice keeps one; the list is newest first.
    call(c, "acme", "PUT", &item(&mug), Some(&ada), Value::Null, 204).await;
    call(c, "acme", "PUT", &item(&mug), Some(&ada), Value::Null, 204).await;
    call(c, "acme", "PUT", &item(&tee), Some(&ada), Value::Null, 204).await;
    call(c, "acme", "PUT", &item(&mug), Some(&bob), Value::Null, 204).await;
    let mine = call(c, "acme", "GET", list, Some(&ada), Value::Null, 200).await;
    let titles: Vec<&str> = mine
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Tee", "Mug"], "{mine}");
    assert_eq!(mine[0]["price_minor"], 1500);

    // Staff see what is saved most; another shop sees nothing.
    let most = call(
        c,
        "acme",
        "GET",
        "/api/wishlists/most-saved",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        most[0],
        json!({ "product_id": mug, "title": "Mug", "saves": 2 })
    );
    let none = call(
        c,
        "other",
        "GET",
        "/api/wishlists/most-saved",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(none, json!([]));

    // Removed, it is gone; a product taken off sale drops out of the list.
    call(
        c,
        "acme",
        "DELETE",
        &item(&mug),
        Some(&ada),
        Value::Null,
        204,
    )
    .await;
    call(
        c,
        "acme",
        "DELETE",
        &item(&mug),
        Some(&ada),
        Value::Null,
        204,
    )
    .await;
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/products/{tee}"),
        a,
        json!({ "status": "draft" }),
        200,
    )
    .await;
    let mine = call(c, "acme", "GET", list, Some(&ada), Value::Null, 200).await;
    assert_eq!(mine, json!([]));
    let events = call(c, "acme", "GET", "/api/events", a, Value::Null, 200).await;
    let text = events.to_string();
    assert!(text.contains("wishlist.item_added") && text.contains("wishlist.item_removed"));

    // In the storefront: the product page's button saves and unsaves it,
    // and the account shows the list.
    let me = format!("soat_customer={bob}");
    let page = |path: String, cookie: Option<String>| {
        let req = c.get(&path).header("x-soat-tenant", "acme");
        let req = match cookie {
            Some(v) => req.header("cookie", &v),
            None => req,
        };
        async move { req.send().await.text() }
    };
    let html = page("/products/mug".into(), Some(me.clone())).await;
    assert!(html.contains("♥ Saved"), "{html}");
    let res = c
        .post("/products/mug/wishlist")
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&json!({}))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/products/mug"));
    assert!(page("/products/mug".into(), Some(me.clone()))
        .await
        .contains("♡ Save for later"));
    c.post("/products/mug/wishlist")
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .form(&json!({}))
        .send()
        .await
        .assert_status(303);
    let html = page("/account/wishlist".into(), Some(me.clone())).await;
    assert!(html.contains("Mug") && html.contains("15.00"), "{html}");
    assert!(page("/account".into(), Some(me))
        .await
        .contains("/account/wishlist"));
    let res = c
        .post("/products/mug/wishlist")
        .header("x-soat-tenant", "acme")
        .form(&json!({}))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/account/login"));
}
