//! C4: tax zones, rates and EU VAT.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    shop: &str,
    token: &str,
    method: &str,
    path: &str,
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
    .header("x-soat-tenant", shop)
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 || status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

fn settings(origin: &str, inclusive: bool, oss: bool) -> Value {
    json!({
        "origin_country": origin,
        "prices_include_tax": inclusive,
        "charge_destination_vat": oss,
    })
}

async fn quote(client: &TestClient, token: &str, body: Value) -> Value {
    send(client, "acme", token, "POST", "/api/tax/quote", body, 200).await
}

#[tokio::test]
async fn eu_vat_preset_and_origin_or_destination_rates() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let s = send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/tax/settings",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(s["prices_include_tax"], true);
    assert!(s["origin_country"].is_null());
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/settings",
        settings("de", true, false),
        200,
    )
    .await;

    let made = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/presets/eu-vat",
        Value::Null,
        201,
    )
    .await;
    assert_eq!(made["items"].as_array().unwrap().len(), 27);
    let again = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/presets/eu-vat",
        Value::Null,
        201,
    )
    .await;
    assert!(again["items"].as_array().unwrap().is_empty());
    let zones = send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/tax/zones",
        Value::Null,
        200,
    )
    .await;
    let fr = zones["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|z| z["countries"] == json!(["FR"]))
        .unwrap()
        .clone();
    assert_eq!(fr["rates"][0]["tax_class"], "reduced");
    assert_eq!(fr["rates"][0]["name"], "VAT 5,5%");

    // Gross prices: 119.00 in Germany holds 19.00 VAT.
    let q = quote(
        &client,
        &t,
        json!({ "country": "DE", "lines": [
            { "amount_minor": 11900 },
            { "tax_class": "reduced", "amount_minor": 1070 },
            { "tax_class": "exempt", "amount_minor": 500 },
            { "tax_class": "unknown-class", "amount_minor": 11900 },
        ]}),
    )
    .await;
    assert_eq!(q["tax_country"], "DE");
    let taxes: Vec<i64> = q["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["tax_minor"].as_i64().unwrap())
        .collect();
    assert_eq!(taxes, [1900, 70, 0, 1900], "unknown classes use standard");
    assert_eq!(q["tax_minor"], 3870);

    // Without OSS, a French buyer pays German VAT; with it, French VAT.
    let body = json!({ "country": "FR", "lines": [{ "amount_minor": 12000 }] });
    let q = quote(&client, &t, body.clone()).await;
    assert_eq!(q["tax_country"], "DE");
    assert_eq!(q["tax_minor"], 1916);
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/settings",
        settings("DE", true, true),
        200,
    )
    .await;
    let q = quote(&client, &t, body).await;
    assert_eq!(q["tax_country"], "FR");
    assert_eq!(q["tax_minor"], 2000);

    // Exports outside every zone are not taxed.
    let q = quote(
        &client,
        &t,
        json!({ "country": "US", "lines": [{ "amount_minor": 12000 }] }),
    )
    .await;
    assert!(q["zone_id"].is_null());
    assert_eq!(q["tax_minor"], 0);
}

#[tokio::test]
async fn business_buyers_in_other_states_reverse_charge() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    send(
        &client,
        "acme",
        &t,
        "PUT",
        "/api/tax/settings",
        settings("DE", false, true),
        200,
    )
    .await;
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/presets/eu-vat",
        Value::Null,
        201,
    )
    .await;
    let line = json!([{ "amount_minor": 10000 }]);
    let q = quote(
        &client,
        &t,
        json!({ "country": "AT", "vat_number": "ATU12345678", "lines": line }),
    )
    .await;
    assert_eq!(q["reverse_charge"], true);
    assert_eq!(q["tax_minor"], 0);
    // Net prices: Austrian VAT is added on top.
    let q = quote(&client, &t, json!({ "country": "AT", "lines": line })).await;
    assert_eq!(q["reverse_charge"], false);
    assert_eq!(q["tax_minor"], 2000);
    // Domestic business buyers still pay VAT.
    let q = quote(
        &client,
        &t,
        json!({ "country": "DE", "vat_number": "DE123456789", "lines": line }),
    )
    .await;
    assert_eq!(q["reverse_charge"], false);
    assert_eq!(q["tax_minor"], 1900);
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        json!({ "country": "AT", "vat_number": "nonsense", "lines": line }),
        422,
    )
    .await;
}

#[tokio::test]
async fn zones_are_managed_and_items_use_their_products_class() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let z = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/zones",
        json!({ "name": "Switzerland", "countries": ["ch", "LI"], "rates": [
            { "tax_class": "standard", "rate_bps": 810, "name": "MWST" },
            { "tax_class": "books", "rate_bps": 260 },
        ]}),
        201,
    )
    .await;
    assert!(z["id"].as_str().unwrap().starts_with("txz_"));
    assert_eq!(z["countries"], json!(["CH", "LI"]));
    let id = z["id"].as_str().unwrap();
    for bad in [
        json!({ "name": "X", "countries": ["CH"], "rates": [] }),
        json!({ "name": "Switzerland", "countries": [], "rates": [] }),
    ] {
        send(&client, "acme", &t, "POST", "/api/tax/zones", bad, 409).await;
    }
    for bad in [
        json!({ "name": "Y", "countries": ["CHE"], "rates": [] }),
        json!({ "name": "Y", "countries": [], "rates": [{ "tax_class": "standard", "rate_bps": 10001 }] }),
        json!({ "name": "Y", "countries": [], "rates": [{ "tax_class": "Std", "rate_bps": 1 }] }),
    ] {
        send(&client, "acme", &t, "POST", "/api/tax/zones", bad, 422).await;
    }

    let book = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "B", "title": "Book", "price_minor": 1000, "currency": "CHF", "tax_class": "books" }),
        201,
    )
    .await;
    assert_eq!(book["tax_class"], "books");
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/products",
        json!({ "sku": "C", "title": "C", "price_minor": 1, "currency": "CHF", "tax_class": "No" }),
        422,
    )
    .await;
    let q = quote(
        &client,
        &t,
        json!({ "country": "CH", "lines": [{ "item_id": book["id"], "amount_minor": 1026 }] }),
    )
    .await;
    assert_eq!(q["lines"][0]["rate_bps"], 260);
    assert_eq!(q["tax_minor"], 26);
    send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/quote",
        json!({ "country": "CH", "lines": [{ "item_id": "prod_nope", "amount_minor": 1 }] }),
        422,
    )
    .await;

    let z = send(
        &client,
        "acme",
        &t,
        "PATCH",
        &format!("/api/tax/zones/{id}"),
        json!({ "countries": ["CH"], "rates": [{ "tax_class": "standard", "rate_bps": 800 }] }),
        200,
    )
    .await;
    assert_eq!(z["countries"], json!(["CH"]));
    assert_eq!(z["rates"].as_array().unwrap().len(), 1);
    send(
        &client,
        "acme",
        &t,
        "DELETE",
        &format!("/api/tax/zones/{id}"),
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        "acme",
        &t,
        "GET",
        &format!("/api/tax/zones/{id}"),
        Value::Null,
        404,
    )
    .await;
    let events = send(
        &client,
        "acme",
        &t,
        "GET",
        "/api/events?kind=tax_zone.deleted",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 1);
}

#[tokio::test]
async fn another_shops_zones_are_not_found() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let z = send(
        &client,
        "acme",
        &t,
        "POST",
        "/api/tax/zones",
        json!({ "name": "DE", "countries": ["DE"], "rates": [{ "tax_class": "standard", "rate_bps": 1900 }] }),
        201,
    )
    .await;
    let path = format!("/api/tax/zones/{}", z["id"].as_str().unwrap());
    send(&client, "globex", &t, "GET", &path, Value::Null, 404).await;
    send(
        &client,
        "globex",
        &t,
        "PATCH",
        &path,
        json!({ "name": "Mine" }),
        404,
    )
    .await;
    send(&client, "globex", &t, "DELETE", &path, Value::Null, 404).await;
    let zones = send(
        &client,
        "globex",
        &t,
        "GET",
        "/api/tax/zones",
        Value::Null,
        200,
    )
    .await;
    assert!(zones["items"].as_array().unwrap().is_empty());
    // globex may use the same country: zones are per shop.
    send(
        &client,
        "globex",
        &t,
        "POST",
        "/api/tax/zones",
        json!({ "name": "DE", "countries": ["DE"], "rates": [] }),
        201,
    )
    .await;
    let q = quote(
        &client,
        &t,
        json!({ "country": "DE", "lines": [{ "amount_minor": 11900 }] }),
    )
    .await;
    assert_eq!(q["tax_minor"], 1900);
}
