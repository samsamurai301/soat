//! B12: digital products, downloaded once paid, with nothing to ship.

mod common;

use serde_json::{json, Value};
use zapline::test::{MultipartForm, TestClient};

/// A request with optional staff bearer and extra headers.
async fn call(
    client: &TestClient,
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
    .header("x-soat-tenant", "acme");
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = headers.iter().fold(req, |r, (k, v)| r.header(*k, *v));
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 300 || status == 204 {
        Value::Null
    } else {
        res.json()
    }
}

async fn dispatch(client: &TestClient) {
    for _ in 0..3 {
        soat_events::dispatch_resources(&client.resources().await)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn digital_products_download_once_paid() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let staff = |method: &'static str, path: String, body: Value, status: u16| {
        let (client, admin) = (&client, &admin);
        async move { call(client, method, &path, Some(admin), &[], body, status).await }
    };
    staff(
        "PUT",
        "/api/tax/settings".into(),
        json!({ "origin_country": "DE", "prices_include_tax": true, "charge_destination_vat": true }),
        200,
    )
    .await;
    staff("POST", "/api/tax/presets/eu-vat".into(), Value::Null, 201).await;
    let invoice = staff(
        "POST",
        "/api/payment-methods".into(),
        json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
        201,
    )
    .await["id"]
        .clone();

    // An e-book: nothing ships, a PDF to download.
    let book = staff(
        "POST",
        "/api/products".into(),
        json!({ "sku": "EBOOK", "title": "Field guide", "price_minor": 900, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await;
    assert_eq!(book["requires_shipping"], false);
    let book = book["id"].as_str().unwrap().to_owned();
    let file = client
        .post("/api/files")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .multipart(MultipartForm::new().file(
            "file",
            "Field Guide.pdf",
            "application/octet-stream",
            &b"%PDF-1.7 a book"[..],
        ))
        .send()
        .await;
    let file: Value = file.assert_status(201).json();
    let downloads = staff(
        "PUT",
        format!("/api/products/{book}/downloads"),
        json!({ "file_ids": [file["id"]] }),
        200,
    )
    .await;
    assert_eq!(downloads[0]["file_name"], "field-guide.pdf");
    staff(
        "PUT",
        format!("/api/products/{book}/downloads"),
        json!({ "file_ids": ["file_nope"] }),
        422,
    )
    .await;
    // Not stocked anywhere, yet it sells: digital items are untracked.
    let stock = staff("GET", format!("/api/inventory/{book}"), Value::Null, 200).await;
    assert_eq!(stock["tracked"], false);

    // A cart with only the book needs no shipping, only a billing address.
    let c = call(
        &client,
        "POST",
        "/api/storefront/carts",
        None,
        &[],
        json!({ "currency": "EUR" }),
        201,
    )
    .await;
    let id = c["cart"]["id"].as_str().unwrap().to_owned();
    let token = c["token"].as_str().unwrap().to_owned();
    let cart = [("x-soat-cart-token", token.as_str())];
    call(
        &client,
        "POST",
        &format!("/api/storefront/carts/{id}/lines"),
        None,
        &cart,
        json!({ "item_id": book, "quantity": 1 }),
        200,
    )
    .await;
    let path = format!("/api/storefront/carts/{id}/checkout");
    let v = call(
        &client,
        "PATCH",
        &path,
        None,
        &cart,
        json!({ "email": "ada@example.com" }),
        200,
    )
    .await;
    assert_eq!(v["requires_shipping"], false);
    assert_eq!(v["missing"], json!(["billing_address"]));
    let v = call(
        &client,
        "PATCH",
        &path,
        None,
        &cart,
        json!({ "billing_address": { "line1": "Rue 1", "postal_code": "75001", "city": "Paris", "country": "FR" } }),
        200,
    )
    .await;
    assert_eq!(v["ready"], true);
    // Taxed where the buyer lives: 20% French VAT held in 9.00.
    assert_eq!(v["tax_minor"], 150);
    assert_eq!(v["total_minor"], 900);
    let done = call(
        &client,
        "POST",
        &format!("{path}/complete"),
        None,
        &cart,
        json!({ "payment_method_id": invoice }),
        201,
    )
    .await;
    let order = done["order"]["id"].as_str().unwrap().to_owned();
    let guest = done["order_token"].as_str().unwrap().to_owned();
    assert_eq!(done["order"]["lines"][0]["requires_shipping"], false);
    assert!(done["order"]["shipping_name"].is_null());
    let mine = [("x-soat-order-token", guest.as_str())];
    let downloads = format!("/api/storefront/orders/{order}/downloads");

    // Unpaid: no downloads yet.
    call(&client, "GET", &downloads, None, &mine, Value::Null, 409).await;
    call(
        &client,
        "GET",
        &downloads,
        None,
        &[("x-soat-order-token", "soat_order_wrong")],
        Value::Null,
        404,
    )
    .await;

    // Paid: the book is delivered and its file opens.
    let pay = done["payment"]["id"].as_str().unwrap().to_owned();
    staff(
        "POST",
        format!("/api/payments/{pay}/mark-paid"),
        Value::Null,
        200,
    )
    .await;
    dispatch(&client).await;
    let o = staff("GET", format!("/api/orders/{order}"), Value::Null, 200).await;
    assert_eq!(o["financial_status"], "paid");
    assert_eq!(o["fulfillment_status"], "fulfilled");
    assert_eq!(o["fulfillments"][0]["carrier"], "digital");
    assert_eq!(o["fulfillments"][0]["status"], "delivered");
    let got = call(&client, "GET", &downloads, None, &mine, Value::Null, 200).await;
    assert_eq!(got.as_array().unwrap().len(), 1);
    assert_eq!(got[0]["title"], "Field guide");
    assert_eq!(got[0]["file"]["content_type"], "application/pdf");
    assert!(got[0]["file"]["url"]
        .as_str()
        .unwrap()
        .contains("field-guide"));
    let staff_view = staff(
        "GET",
        format!("/api/orders/{order}/downloads"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(staff_view[0]["file"]["id"], file["id"]);
    // Paying again does not deliver twice.
    dispatch(&client).await;
    let o = staff("GET", format!("/api/orders/{order}"), Value::Null, 200).await;
    assert_eq!(o["fulfillments"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_shipped_item_still_needs_shipping() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let staff = |method: &'static str, path: String, body: Value, status: u16| {
        let (client, admin) = (&client, &admin);
        async move { call(client, method, &path, Some(admin), &[], body, status).await }
    };
    let book = staff(
        "POST",
        "/api/products".into(),
        json!({ "sku": "EBOOK", "title": "E-book", "price_minor": 900, "currency": "EUR",
                "requires_shipping": false }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tee = staff(
        "POST",
        "/api/products".into(),
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 1500, "currency": "EUR" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    staff(
        "PATCH",
        format!("/api/inventory/{tee}"),
        json!({ "tracked": false }),
        200,
    )
    .await;
    let c = call(
        &client,
        "POST",
        "/api/storefront/carts",
        None,
        &[],
        json!({ "currency": "EUR" }),
        201,
    )
    .await;
    let id = c["cart"]["id"].as_str().unwrap().to_owned();
    let token = c["token"].as_str().unwrap().to_owned();
    let cart = [("x-soat-cart-token", token.as_str())];
    for item in [&book, &tee] {
        call(
            &client,
            "POST",
            &format!("/api/storefront/carts/{id}/lines"),
            None,
            &cart,
            json!({ "item_id": item, "quantity": 1 }),
            200,
        )
        .await;
    }
    let v = call(
        &client,
        "PATCH",
        &format!("/api/storefront/carts/{id}/checkout"),
        None,
        &cart,
        json!({ "email": "ada@example.com" }),
        200,
    )
    .await;
    assert_eq!(v["requires_shipping"], true);
    assert_eq!(v["missing"], json!(["shipping_address"]));
    // A product can be switched back to shipping.
    let p = staff(
        "PATCH",
        format!("/api/products/{book}"),
        json!({ "requires_shipping": true }),
        200,
    )
    .await;
    assert_eq!(p["requires_shipping"], true);
}

#[tokio::test]
async fn downloads_stay_in_their_shop() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let theirs: Value = {
        let res = client
            .post("/api/products")
            .header("x-soat-tenant", "globex")
            .bearer(&admin)
            .json(
                &json!({ "sku": "EBOOK", "title": "E", "price_minor": 1, "currency": "EUR",
                           "requires_shipping": false }),
            )
            .send()
            .await;
        res.assert_status(201).json()
    };
    let path = format!("/api/products/{}/downloads", theirs["id"].as_str().unwrap());
    call(&client, "GET", &path, Some(&admin), &[], Value::Null, 404).await;
    call(
        &client,
        "PUT",
        &path,
        Some(&admin),
        &[],
        json!({ "file_ids": [] }),
        404,
    )
    .await;
}
