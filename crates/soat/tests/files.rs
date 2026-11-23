//! B6: files and product media.

mod common;

use serde_json::{json, Value};
use zapline::test::{MultipartForm, TestClient};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR not really a picture";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0 also not a picture";

async fn upload(
    client: &TestClient,
    token: &str,
    shop: &str,
    name: &str,
    bytes: &'static [u8],
    status: u16,
) -> Value {
    let res = client
        .post("/api/files")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .multipart(
            MultipartForm::new()
                .text("alt", "A red shoe")
                // The client's content type is ignored: the bytes decide.
                .file("file", name, "text/html", bytes),
        )
        .send()
        .await;
    let res = res.assert_status(status);
    if status == 201 {
        res.json()
    } else {
        Value::Null
    }
}

async fn product(client: &TestClient, token: &str, shop: &str) -> String {
    let p: Value = client
        .post("/api/products")
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201)
        .json();
    p["id"].as_str().unwrap().to_owned()
}

async fn set_media(
    client: &TestClient,
    token: &str,
    shop: &str,
    id: &str,
    ids: Value,
    status: u16,
) -> Value {
    let res = client
        .put(&format!("/api/products/{id}/media"))
        .header("x-soat-tenant", shop)
        .bearer(token)
        .json(&json!({ "file_ids": ids }))
        .send()
        .await;
    let res = res.assert_status(status);
    if status == 200 {
        res.json()
    } else {
        Value::Null
    }
}

#[tokio::test]
async fn uploads_are_checked_by_content_and_served_by_signed_link() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let file = upload(&client, &admin, "acme", "../Red Shoe.HTML", PNG, 201).await;
    assert!(file["id"].as_str().unwrap().starts_with("file_"));
    assert_eq!(file["file_name"], "red-shoe.png");
    assert_eq!(file["content_type"], "image/png");
    assert_eq!(file["size"], PNG.len());
    assert_eq!(file["alt"], "A red shoe");

    let res = client.get(file["url"].as_str().unwrap()).send().await;
    let res = res.assert_status(200);
    assert_eq!(res.bytes().as_ref(), PNG);
    assert_eq!(res.header("content-type"), Some("image/png"));
    // Without its signature the link is useless.
    let bare = file["url"]
        .as_str()
        .unwrap()
        .split('?')
        .next()
        .unwrap()
        .to_owned();
    assert_ne!(client.get(&bare).send().await.status(), 200);

    upload(
        &client,
        &admin,
        "acme",
        "x.svg",
        b"<svg onload=alert(1)>",
        415,
    )
    .await;
    upload(&client, &admin, "acme", "x.png", b"<!doctype html>", 415).await;

    let id = file["id"].as_str().unwrap();
    let changed: Value = client
        .patch(&format!("/api/files/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "alt": "Shoe, side view" }))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(changed["alt"], "Shoe, side view");
    let list: Value = client
        .get("/api/files?content_type=image/png")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(list["total"], 1);
}

#[tokio::test]
async fn products_show_media_in_order() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let a = upload(&client, &admin, "acme", "a.png", PNG, 201).await;
    let b = upload(&client, &admin, "acme", "b.jpg", JPEG, 201).await;
    let (a, b) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());
    let id = product(&client, &admin, "acme").await;

    let media = set_media(&client, &admin, "acme", &id, json!([b, a]), 200).await;
    let names: Vec<&str> = media
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["b.jpg", "a.png"]);
    set_media(&client, &admin, "acme", &id, json!([a, a]), 422).await;
    set_media(&client, &admin, "acme", &id, json!(["file_nope"]), 422).await;

    // Deleting a file takes it off the product.
    client
        .delete(&format!("/api/files/{b}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(204);
    let media: Value = client
        .get(&format!("/api/products/{id}/media"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(media.as_array().unwrap().len(), 1);
    assert_eq!(media[0]["id"], a);
}

#[tokio::test]
async fn files_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = upload(&client, &admin, "globex", "g.png", PNG, 201).await;
    let theirs = theirs["id"].as_str().unwrap();
    for req in [
        client.get(&format!("/api/files/{theirs}")),
        client.delete(&format!("/api/files/{theirs}")),
    ] {
        req.header("x-soat-tenant", "acme")
            .bearer(&admin)
            .send()
            .await
            .assert_status(404);
    }
    let ours = product(&client, &admin, "acme").await;
    set_media(&client, &admin, "acme", &ours, json!([theirs]), 422).await;
    let list: Value = client
        .get("/api/files")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(list["total"], 0);
}
