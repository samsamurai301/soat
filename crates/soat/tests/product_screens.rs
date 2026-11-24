//! The admin's product page: options, variants and media.

mod common;

use serde_json::{json, Value};
use zapline::test::{MultipartForm, TestClient};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR not really a picture";

async fn sign_in(client: &TestClient) -> String {
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    res.assert_status(303);
    format!("soat_admin={}", res.cookie("soat_admin").unwrap().value())
}

#[tokio::test]
async fn staff_manage_variants_and_media() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let product: Value = client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "sku": "TEE", "title": "Tee", "price_minor": 2900, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    let id = product["id"].as_str().unwrap().to_owned();
    let cookie = sign_in(&client).await;
    let page = format!("/admin/acme/products/{id}");
    let post = |path: String, form: Value| {
        client
            .post(&path)
            .header("cookie", &cookie)
            .form(&form)
            .send()
    };
    let api = |path: String| {
        client
            .get(&path)
            .header("x-soat-tenant", "acme")
            .bearer(&admin)
            .send()
    };

    // Options as lines; a line without values is explained, not saved.
    let res = post(format!("{page}/options"), json!({ "options": "Size" })).await;
    res.assert_status(422);
    assert!(res.text().contains("a name, a colon and values"));
    let res = post(
        format!("{page}/options"),
        json!({ "options": "Size: S, M\nColor: Red, Blue" }),
    )
    .await;
    res.assert_status(303);
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Size: S, M\nColor: Red, Blue"));
    assert!(html.contains("Add the 4 missing variants"));

    // One by hand, at its own price; then the rest at once.
    post(
        format!("{page}/variants"),
        json!({ "value0": "S", "value1": "Red", "price": "31,50" }),
    )
    .await
    .assert_status(303);
    post(format!("{page}/variants/all"), json!({}))
        .await
        .assert_status(303);
    let variants: Value = api(format!("/api/products/{id}/variants")).await.json();
    let list = variants.as_array().unwrap();
    assert_eq!(list.len(), 4);
    assert_eq!(list[0]["sku"], "TEE-S-RED");
    assert_eq!(list[0]["price_minor"], 3150);
    assert_eq!(list[3]["sku"], "TEE-M-BLUE");
    assert_eq!(list[3]["price_minor"], 2900);
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(!html.contains("missing variants"));
    assert!(html.contains("value=\"31.50\""));

    // Price and SKU per variant; a bad price is explained.
    let v = list[1]["id"].as_str().unwrap();
    let res = post(
        format!("{page}/variants/{v}/save"),
        json!({ "sku": "TEE-S-BLUE", "price": "abc" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("not a price in EUR"));
    post(
        format!("{page}/variants/{v}/save"),
        json!({ "sku": "TEE-SB", "price": "30" }),
    )
    .await
    .assert_status(303);
    post(format!("{page}/variants/{v}/delete"), json!({}))
        .await
        .assert_status(303);
    let variants: Value = api(format!("/api/products/{id}/variants")).await.json();
    assert_eq!(variants.as_array().unwrap().len(), 3);
    // An option still in use cannot go.
    let res = post(
        format!("{page}/options"),
        json!({ "options": "Size: S, M" }),
    )
    .await;
    res.assert_status(422);

    // Media: upload two, describe, reorder, remove.
    for (name, alt) in [("front.png", "Front"), ("back.png", "")] {
        client
            .post(&format!("{page}/media"))
            .header("cookie", &cookie)
            .multipart(MultipartForm::new().text("alt", alt).file(
                "file",
                name,
                "image/png",
                PNG.to_vec(),
            ))
            .send()
            .await
            .assert_status(303);
    }
    let res = client
        .post(&format!("{page}/media"))
        .header("cookie", &cookie)
        .multipart(MultipartForm::new().file("file", "x.html", "text/html", b"<html>".to_vec()))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.text().contains("Upload a PNG"));
    let media: Value = api(format!("/api/products/{id}/media")).await.json();
    let files: Vec<String> = media
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(files.len(), 2);
    assert_eq!(media[0]["alt"], "Front");
    assert_eq!(media[1]["file_name"], "back.png");
    post(
        format!("{page}/media/{}/alt", files[1]),
        json!({ "alt": "Back print" }),
    )
    .await
    .assert_status(303);
    post(format!("{page}/media/{}/up", files[1]), json!({}))
        .await
        .assert_status(303);
    let media: Value = api(format!("/api/products/{id}/media")).await.json();
    assert_eq!(media[0]["id"], files[1].as_str());
    assert_eq!(media[0]["alt"], "Back print");
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("alt=\"Back print\""));
    post(format!("{page}/media/{}/remove", files[0]), json!({}))
        .await
        .assert_status(303);
    let media: Value = api(format!("/api/products/{id}/media")).await.json();
    assert_eq!(media.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn staff_sell_digital_products() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let cookie = sign_in(&client).await;
    let res = client
        .post("/admin/acme/products")
        .header("cookie", &cookie)
        .form(
            &json!({ "title": "Field guide", "sku": "BOOK", "price": "9", "currency": "EUR",
                       "status": "active", "digital": "on" }),
        )
        .send()
        .await;
    res.assert_status(303);
    let page = res.header("location").unwrap().to_owned();
    let id = page
        .rsplit('/')
        .next()
        .unwrap()
        .split(['?', '#'])
        .next()
        .unwrap()
        .to_owned();
    let product: Value = client
        .get(&format!("/api/products/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    assert_eq!(product["requires_shipping"], false);
    let page = format!("/admin/acme/products/{id}");
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Downloads"));
    assert!(html.contains("name=\"digital\" checked"));

    let res = client
        .post(&format!("{page}/downloads"))
        .header("cookie", &cookie)
        .multipart(MultipartForm::new().file(
            "file",
            "Guide.pdf",
            "application/pdf",
            &b"%PDF-1.7 the guide"[..],
        ))
        .send()
        .await;
    res.assert_status(303);
    let html = client
        .get(&page)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("guide.pdf"));
    let files: Value = client
        .get(&format!("/api/products/{id}/downloads"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    let file = files[0]["id"].as_str().unwrap();
    client
        .post(&format!("{page}/downloads/{file}/remove"))
        .header("cookie", &cookie)
        .form(&json!({}))
        .send()
        .await
        .assert_status(303);
    let files: Value = client
        .get(&format!("/api/products/{id}/downloads"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    assert_eq!(files, json!([]));
}
