//! D5–D6: pages built from blocks, and menus.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    t: &str,
    shop: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PUT" => client.put(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop)
    .bearer(t);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    res.assert_status(status);
    if status >= 300 || status == 204 {
        if status >= 400 {
            res.json()
        } else {
            Value::Null
        }
    } else {
        res.json()
    }
}

async fn storefront(client: &TestClient, shop: &str, path: &str, status: u16) -> Value {
    let res = client.get(path).header("x-soat-tenant", shop).send().await;
    res.assert_status(status);
    if status == 200 {
        res.json()
    } else {
        Value::Null
    }
}

fn about() -> Value {
    json!({
        "title": "About Us",
        "seo_description": "Who we are",
        "blocks": [
            { "type": "heading", "text": "About us", "level": 1 },
            { "type": "text", "body": "We make *shoes*." },
            { "type": "button", "label": "Shop now", "url": "/collections/all" }
        ]
    })
}

#[tokio::test]
async fn pages_are_drafts_until_published() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let p = send(&client, &t, "acme", "POST", "/api/pages", about(), 201).await;
    let id = p["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("page_"));
    assert_eq!(p["handle"], "about-us");
    assert_eq!(p["published"], false);
    assert_eq!(p["blocks"].as_array().unwrap().len(), 3);
    storefront(&client, "acme", "/api/storefront/pages/about-us", 404).await;

    let mut publish = about();
    publish["published"] = json!(true);
    let p = send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/pages/{id}"),
        publish.clone(),
        200,
    )
    .await;
    assert_eq!(p["published"], true);
    let first = p["published_at"].clone();
    assert!(first.is_string());
    let shown = storefront(&client, "acme", "/api/storefront/pages/about-us", 200).await;
    assert_eq!(shown["blocks"][0]["text"], "About us");
    assert_eq!(shown["seo_description"], "Who we are");
    // Saving again keeps the first publication time.
    let p = send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/pages/{id}"),
        publish,
        200,
    )
    .await;
    assert_eq!(p["published_at"], first);

    // Another shop has no such page, by id or by handle.
    send(
        &client,
        &t,
        "globex",
        "GET",
        &format!("/api/pages/{id}"),
        Value::Null,
        404,
    )
    .await;
    storefront(&client, "globex", "/api/storefront/pages/about-us", 404).await;

    // Handles are unique per shop, not across shops.
    let e = send(&client, &t, "acme", "POST", "/api/pages", about(), 409).await;
    assert_eq!(e["code"], "handle_taken");
    send(&client, &t, "globex", "POST", "/api/pages", about(), 201).await;

    // A bad block is named.
    let e = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/pages",
        json!({ "title": "X", "blocks": [{ "type": "text", "body": "ok" }, { "type": "button", "label": "Go", "url": "javascript:alert(1)" }] }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_content");
    assert!(e.to_string().contains("blocks[1].url"), "{e}");
    let e = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/pages",
        json!({ "title": "X", "handle": "Not A Handle" }),
        422,
    )
    .await;
    assert_eq!(e["code"], "invalid_content");

    let list = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/pages?q=about",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 1);

    // Unpublished again, it leaves the storefront.
    send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/pages/{id}"),
        about(),
        200,
    )
    .await;
    storefront(&client, "acme", "/api/storefront/pages/about-us", 404).await;

    send(
        &client,
        &t,
        "acme",
        "DELETE",
        &format!("/api/pages/{id}"),
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "GET",
        &format!("/api/pages/{id}"),
        Value::Null,
        404,
    )
    .await;
    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?kind=page.deleted",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 1);
}

#[tokio::test]
async fn menus_link_the_shop() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let main = json!({
        "title": "Main",
        "items": [
            { "title": "Shoes", "url": "/collections/shoes", "items": [
                { "title": "Trail", "url": "/collections/trail" }
            ] },
            { "title": "About", "url": "/pages/about-us" },
            { "title": "Blog", "url": "https://blog.example.com" }
        ]
    });
    let m = send(&client, &t, "acme", "POST", "/api/menus", main.clone(), 201).await;
    let id = m["id"].as_str().unwrap().to_owned();
    assert_eq!(m["handle"], "main");
    let shown = storefront(&client, "acme", "/api/storefront/menus/main", 200).await;
    assert_eq!(shown["items"][0]["items"][0]["title"], "Trail");
    storefront(&client, "globex", "/api/storefront/menus/main", 404).await;
    send(
        &client,
        &t,
        "globex",
        "PUT",
        &format!("/api/menus/{id}"),
        main.clone(),
        404,
    )
    .await;

    let e = send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/menus/{id}"),
        json!({ "title": "Main", "items": [{ "title": "Bad", "url": "//evil.example" }] }),
        422,
    )
    .await;
    assert!(e.to_string().contains("items[0].url"), "{e}");

    let footer = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/menus",
        json!({ "title": "Footer links", "handle": "footer" }),
        201,
    )
    .await;
    assert_eq!(footer["items"], json!([]));
    let list = send(&client, &t, "acme", "GET", "/api/menus", Value::Null, 200).await;
    let handles: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles, ["footer", "main"]);

    // A key that may only read content cannot change it.
    let key: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&t)
        .json(&json!({ "name": "reader", "permissions": ["content:read"] }))
        .send()
        .await
        .assert_status(201)
        .json();
    let secret = key["secret"].as_str().unwrap();
    client
        .get("/api/menus")
        .bearer(secret)
        .send()
        .await
        .assert_status(200);
    client
        .post("/api/menus")
        .bearer(secret)
        .json(&main)
        .send()
        .await
        .assert_status(403);

    send(
        &client,
        &t,
        "acme",
        "DELETE",
        &format!("/api/menus/{id}"),
        Value::Null,
        204,
    )
    .await;
    storefront(&client, "acme", "/api/storefront/menus/main", 404).await;
}
