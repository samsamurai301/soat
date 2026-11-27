//! I9: product feeds for Google Merchant Center and Meta catalogs.
//! One item per product or variant, with prices, stock and problems.
#![cfg(feature = "feeds")]

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
        "PATCH" => client.patch(path),
        "PUT" => client.put(path),
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

#[tokio::test]
async fn feeds_list_products_for_google_and_meta() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
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
        "/api/domains",
        a,
        json!({ "domain": "shop.acme.example" }),
        201,
    )
    .await;
    // A mug with a GTIN, a description and stock; a tee in two sizes,
    // the small one sold out; a draft that is not for sale.
    call(
        c,
        "acme",
        "PUT",
        "/api/fields/product/gtin",
        a,
        json!({ "name": "GTIN", "kind": "text" }),
        200,
    )
    .await;
    call(
        c,
        "acme",
        "PUT",
        "/api/fields/product/brand",
        a,
        json!({ "name": "Brand", "kind": "text" }),
        200,
    )
    .await;
    let mug = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "MUG", "title": "Mug & saucer", "price_minor": 1200, "currency": "EUR",
                "status": "active", "description": "A **big** mug.", "weight_grams": 350 }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/products/{mug}/fields"),
        a,
        json!({ "gtin": "4006381333931", "brand": "Kaffee & Co" }),
        200,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        &format!("/api/inventory/{mug}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 5, "reason": "received" }),
        200,
    )
    .await;
    let tee = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 2500, "currency": "EUR",
                "status": "active" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/products/{tee}/options"),
        a,
        json!({ "options": [{ "name": "Size", "values": ["S", "M"] }] }),
        200,
    )
    .await;
    let small = call(
        c,
        "acme",
        "POST",
        &format!("/api/products/{tee}/variants"),
        a,
        json!({ "sku": "TEE-S", "options": ["S"] }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let medium = call(
        c,
        "acme",
        "POST",
        &format!("/api/products/{tee}/variants"),
        a,
        json!({ "sku": "TEE-M", "options": ["M"] }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/inventory/{medium}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 3, "reason": "received" }),
        200,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "SECRET", "title": "Secret", "price_minor": 100, "currency": "EUR", "status": "draft" }),
        201,
    )
    .await;
    // The mug is on sale: 9.90 instead of 12.00.
    let sale = call(
        c,
        "acme",
        "POST",
        "/api/price-lists",
        a,
        json!({ "name": "Sale", "currency": "EUR", "priority": 1 }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/price-lists/{sale}/prices"),
        a,
        json!({ "prices": [{ "item_id": mug, "price_minor": 990, "compare_at_minor": 1200 }] }),
        200,
    )
    .await;

    call(
        c,
        "acme",
        "POST",
        "/api/feeds",
        a,
        json!({ "name": "X", "format": "amazon" }),
        422,
    )
    .await;
    let google = call(
        c,
        "acme",
        "POST",
        "/api/feeds",
        a,
        json!({ "name": "Google DE", "format": "google" }),
        201,
    )
    .await;
    assert_eq!(google["currency"], "EUR");
    let path = google["path"].as_str().unwrap().to_owned();
    assert!(path.starts_with("/feeds/"));

    // Google's RSS: three items (the draft is left out).
    let res = c.get(&path).header("x-soat-tenant", "acme").send().await;
    res.assert_status(200);
    assert!(res
        .header("content-type")
        .unwrap()
        .starts_with("application/xml"));
    let xml = res.text();
    assert_eq!(xml.matches("<item>").count(), 3, "{xml}");
    for part in [
        "<g:title>Mug &amp; saucer</g:title>",
        "<g:description>A big mug.</g:description>",
        "<g:price>12.00 EUR</g:price>",
        "<g:sale_price>9.90 EUR</g:sale_price>",
        "<g:gtin>4006381333931</g:gtin>",
        "<g:brand>Kaffee &amp; Co</g:brand>",
        "<g:shipping_weight>350 g</g:shipping_weight>",
        "<g:mpn>MUG</g:mpn>",
        &format!("<g:id>{small}</g:id><g:item_group_id>{tee}</g:item_group_id><g:title>Tee - S</g:title>"),
        "<g:availability>out_of_stock</g:availability>",
        "<g:availability>in_stock</g:availability>",
        "<g:identifier_exists>no</g:identifier_exists>",
        "<g:link>https://shop.acme.example/products/mug-saucer</g:link>",
    ] {
        assert!(xml.contains(part), "{part} in {xml}");
    }
    assert!(!xml.contains("Secret"));

    // Leaving out what is sold out; turned off, the URL is gone.
    let id = google["id"].as_str().unwrap().to_owned();
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/feeds/{id}"),
        a,
        json!({ "include_out_of_stock": false }),
        200,
    )
    .await;
    let xml = c
        .get(&path)
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .text();
    assert_eq!(xml.matches("<item>").count(), 2, "{xml}");
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/feeds/{id}"),
        a,
        json!({ "active": false }),
        200,
    )
    .await;
    c.get(&path)
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(404);
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/feeds/{id}"),
        a,
        json!({ "active": true }),
        200,
    )
    .await;
    // A new address: the old one stops working.
    let renewed = call(
        c,
        "acme",
        "POST",
        &format!("/api/feeds/{id}/token"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_ne!(renewed["path"], path.as_str());
    c.get(&path)
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(404);
    c.get("/feeds/../etc")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(404);

    // Meta's CSV.
    let meta = call(
        c,
        "acme",
        "POST",
        "/api/feeds",
        a,
        json!({ "name": "Meta", "format": "meta", "brand": "Acme" }),
        201,
    )
    .await;
    let res = c
        .get(meta["path"].as_str().unwrap())
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    assert!(res.header("content-type").unwrap().starts_with("text/csv"));
    let csv = res.text();
    assert_eq!(csv.lines().count(), 4, "{csv}");
    assert!(csv.contains(",out of stock,new,25.00 EUR,"), "{csv}");
    assert!(csv.contains(",Acme,"), "{csv}");

    // The preview names the problems of what the feed lists (the sold-out
    // tee is left out): no pictures, no GTIN or description on the tee.
    let p = call(
        c,
        "acme",
        "GET",
        &format!("/api/feeds/{id}/preview"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(p["count"], 2);
    let codes: Vec<&str> = p["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes.iter().filter(|c| **c == "missing_image").count(), 2);
    assert_eq!(codes.iter().filter(|c| **c == "missing_gtin").count(), 1);
    assert_eq!(
        codes
            .iter()
            .filter(|c| **c == "missing_description")
            .count(),
        1
    );
    assert_eq!(p["errors"], 3);
    assert_eq!(p["warnings"], 1);
    let listed = call(c, "acme", "GET", "/api/feeds", a, Value::Null, 200).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 2);
    assert!(listed["items"][0]["fetched_at"].is_string());

    // The admin page shows both with their problems.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/feeds")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Google DE") && html.contains("Meta CSV") && html.contains("no picture"),
        "{html}"
    );
    let res = c
        .post("/admin/acme/feeds")
        .header("cookie", &cookie)
        .form(&json!({ "name": "Shoes", "format": "google", "collection_handle": "shoes" }))
        .send()
        .await;
    res.assert_status(303);

    // Events never carry the token; another shop sees none of it.
    let e = call(
        c,
        "acme",
        "GET",
        "/api/events?kind=feed.created",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(e["items"].as_array().unwrap().len(), 3);
    assert!(!e.to_string().contains(
        renewed["path"]
            .as_str()
            .unwrap()
            .trim_start_matches("/feeds/")
    ));
    call(
        c,
        "other",
        "GET",
        &format!("/api/feeds/{id}/preview"),
        a,
        Value::Null,
        404,
    )
    .await;
    call(
        c,
        "other",
        "PATCH",
        &format!("/api/feeds/{id}"),
        a,
        json!({ "name": "x" }),
        404,
    )
    .await;
    c.get(renewed["path"].as_str().unwrap())
        .header("x-soat-tenant", "other")
        .send()
        .await
        .assert_status(404);
    let none = call(c, "other", "GET", "/api/feeds", a, Value::Null, 200).await;
    assert!(none["items"].as_array().unwrap().is_empty());
    call(
        c,
        "acme",
        "DELETE",
        &format!("/api/feeds/{id}"),
        a,
        Value::Null,
        204,
    )
    .await;
}
