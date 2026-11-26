//! D10: the shop in more languages: translated texts per resource and a
//! storefront under `/de/...`.

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
    if status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn page(client: &TestClient, shop: &str, path: &str, status: u16) -> String {
    let res = client.get(path).header("x-soat-tenant", shop).send().await;
    res.assert_status(status);
    res.text()
}

#[tokio::test]
async fn shops_choose_their_languages() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let l = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/languages",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(l, json!({ "default_language": "en", "languages": [] }));
    let l = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/languages",
        json!({ "default_language": "EN", "languages": ["de", "fr", "de", "en"] }),
        200,
    )
    .await;
    assert_eq!(
        l,
        json!({ "default_language": "en", "languages": ["de", "fr"] })
    );
    let bad = send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/languages",
        json!({ "default_language": "en", "languages": ["german"] }),
        422,
    )
    .await;
    assert_eq!(bad["code"], "invalid_languages");
    // Another shop is untouched.
    let l = send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/languages",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(l["languages"], json!([]));
}

#[tokio::test]
async fn translations_belong_to_their_shop() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    for shop in ["acme", "globex"] {
        send(
            &client,
            &t,
            shop,
            "PUT",
            "/api/languages",
            json!({ "default_language": "en", "languages": ["de"] }),
            200,
        )
        .await;
    }
    let p = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": "SHOE", "title": "Trail Shoe", "price_minor": 12999, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap();
    let path = format!("/api/translations/product/{id}/de");
    let tr = send(
        &client,
        &t,
        "acme",
        "PUT",
        &path,
        json!({ "fields": { "title": "Trailschuh", "description": "Leicht." } }),
        200,
    )
    .await;
    assert_eq!(tr["items"].as_array().unwrap().len(), 2);

    // Only the shop's other languages, and only known fields.
    let fr = format!("/api/translations/product/{id}/fr");
    send(
        &client,
        &t,
        "acme",
        "PUT",
        &fr,
        json!({ "fields": { "title": "x" } }),
        422,
    )
    .await;
    let bad = json!({ "fields": { "price": "1" } });
    send(&client, &t, "acme", "PUT", &path, bad, 422).await;

    // Another shop's product is not found, and its translations are empty.
    send(
        &client,
        &t,
        "globex",
        "PUT",
        &path,
        json!({ "fields": { "title": "x" } }),
        404,
    )
    .await;
    let all = format!("/api/translations/product/{id}");
    send(&client, &t, "globex", "GET", &all, Value::Null, 404).await;
    let mine = send(&client, &t, "acme", "GET", &all, Value::Null, 200).await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);

    // An empty value removes one field; DELETE removes the language.
    let tr = send(
        &client,
        &t,
        "acme",
        "PUT",
        &path,
        json!({ "fields": { "description": "" } }),
        200,
    )
    .await;
    assert_eq!(tr["items"].as_array().unwrap().len(), 1);
    send(&client, &t, "acme", "DELETE", &path, Value::Null, 204).await;
    let mine = send(&client, &t, "acme", "GET", &all, Value::Null, 200).await;
    assert_eq!(mine["items"], json!([]));

    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?kind=translation.updated",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn buyers_shop_in_german() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let p = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": "SHOE", "title": "Trail Shoe", "price_minor": 12999, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap().to_owned();
    let loc = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
        201,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "POST",
        &format!("/api/inventory/{id}/adjustments"),
        json!({ "location_id": loc["id"], "delta": 5, "reason": "received" }),
        200,
    )
    .await;
    let menu = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/menus",
        json!({ "title": "Main", "items": [{ "title": "About us", "url": "/pages/about" }] }),
        201,
    )
    .await;

    // Before German is published, /de shows the default language.
    let en = page(&client, "acme", "/de/products/trail-shoe", 200).await;
    assert!(en.contains("Add to cart"), "{en}");

    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/languages",
        json!({ "default_language": "en", "languages": ["de"] }),
        200,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/translations/product/{id}/de"),
        json!({ "fields": { "title": "Trailschuh" } }),
        200,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/translations/menu/{}/de", menu["id"].as_str().unwrap()),
        json!({ "fields": { "items": "[{\"title\":\"Über uns\",\"url\":\"/pages/about\"}]" } }),
        200,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/translations/theme/strings/de",
        json!({ "fields": { "search": "Finden" } }),
        200,
    )
    .await;

    let de = page(&client, "acme", "/de/products/trail-shoe", 200).await;
    assert!(de.contains("<html lang=\"de\""), "{de}");
    assert!(de.contains("Trailschuh"));
    assert!(de.contains("In den Warenkorb"), "built-in German texts");
    assert!(de.contains("Finden"), "the shop's own wording wins");
    assert!(de.contains("Über uns") && de.contains("href=\"/de/pages/about\""));
    assert!(de.contains("action=\"/de/cart/add\""));
    assert!(de.contains("hreflang=\"de\""), "alternates");
    assert!(de.contains("/de/products/trail-shoe\""));
    assert!(de.contains("hreflang=\"x-default\""));

    // The default language keeps its plain addresses and texts.
    let en = page(&client, "acme", "/products/trail-shoe", 200).await;
    assert!(en.contains("Trail Shoe") && !en.contains("Trailschuh"));
    assert!(en.contains("href=\"/pages/about\""));
    // The switcher links to the same page in each language.
    assert!(en.contains("href=\"/de/products/trail-shoe\""));

    // Forms work under the prefix and stay in the language.
    let res = client
        .post("/de/cart/add")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "product_id": id }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/de/cart"));

    // The search page and the 404 page too.
    let s = page(&client, "acme", "/de/search?q=trail", 200).await;
    assert!(s.contains("Trailschuh"), "{s}");
    let missing = page(&client, "acme", "/de/nothing-here", 404).await;
    assert!(missing.contains("lang=\"de\""));
    // Account pages send buyers on in their language.
    let res = client
        .get("/de/account")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    assert_eq!(res.header("location"), Some("/de/account/login"));
    let login = page(&client, "acme", "/de/account/login", 200).await;
    assert!(login.contains("<title>Anmelden"), "{login}");
    // A language that is no code is just a missing page.
    page(&client, "acme", "/xyz/products/trail-shoe", 404).await;
}
