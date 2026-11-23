//! D10: markets: countries that buy in one currency, with prices converted
//! at the market's exchange rate.

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
        "PATCH" => client.patch(path),
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

fn swiss() -> Value {
    json!({
        "name": "Switzerland", "countries": ["ch", "LI"], "currency": "CHF",
        "base_currency": "EUR", "exchange_rate_ppm": 950_000, "rounding": "charm"
    })
}

#[tokio::test]
async fn shops_sell_in_markets() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let m = send(&client, &t, "acme", "POST", "/api/markets", swiss(), 201).await;
    let id = m["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("mkt_"));
    assert_eq!(m["countries"], json!(["CH", "LI"]));
    assert_eq!(m["active"], true);

    // A country is in one active market; codes, currencies and rates are
    // checked.
    let mut other = swiss();
    other["name"] = json!("Alps");
    other["countries"] = json!(["AT", "CH"]);
    let e = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/markets",
        other.clone(),
        409,
    )
    .await;
    assert_eq!(e["code"], "country_in_market");
    other["active"] = json!(false);
    send(&client, &t, "acme", "POST", "/api/markets", other, 201).await;
    for bad in [
        json!({ "name": "X", "countries": ["XX"], "currency": "CHF" }),
        json!({ "name": "X", "countries": [], "currency": "CHF" }),
        json!({ "name": "X", "countries": ["SE"], "currency": "sek" }),
        json!({ "name": "X", "countries": ["SE"], "currency": "SEK", "exchange_rate_ppm": 11 }),
        json!({ "name": "X", "countries": ["SE"], "currency": "SEK", "rounding": "up" }),
    ] {
        let e = send(&client, &t, "acme", "POST", "/api/markets", bad, 422).await;
        assert!(
            e["code"] == "invalid_market" || e["code"] == "invalid_currency",
            "{e}"
        );
    }
    let e = send(&client, &t, "acme", "POST", "/api/markets", swiss(), 409).await;
    assert_eq!(e["code"], "country_in_market");

    let list = send(&client, &t, "acme", "GET", "/api/markets", Value::Null, 200).await;
    assert_eq!(list["total"], 2);

    // Another shop sees none of it.
    let path = format!("/api/markets/{id}");
    send(&client, &t, "globex", "GET", &path, Value::Null, 404).await;
    send(
        &client,
        &t,
        "globex",
        "PATCH",
        &path,
        json!({ "name": "Mine" }),
        404,
    )
    .await;
    send(&client, &t, "globex", "DELETE", &path, Value::Null, 404).await;
    let list = send(
        &client,
        &t,
        "globex",
        "GET",
        "/api/markets",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 0);

    let m = send(
        &client,
        &t,
        "acme",
        "PATCH",
        &path,
        json!({ "rounding": "whole" }),
        200,
    )
    .await;
    assert_eq!(m["rounding"], "whole");
    assert_eq!(m["exchange_rate_ppm"], 950_000);

    // Buyers' storefronts find the market by country.
    let res = client
        .get("/api/storefront/markets?country=li")
        .header("x-soat-tenant", "acme")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json::<Value>()["currency"], "CHF");
    client
        .get("/api/storefront/markets?country=DE")
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(404);

    send(&client, &t, "acme", "DELETE", &path, Value::Null, 204).await;
    let events = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/events?kind=market.deleted",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn prices_convert_unless_a_list_prices_them() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let p = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": "SHOE", "title": "Trail Shoe", "price_minor": 2000, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap().to_owned();
    let price = format!("/api/prices/{id}?currency=CHF");
    send(&client, &t, "acme", "GET", &price, Value::Null, 404).await;

    let m = send(&client, &t, "acme", "POST", "/api/markets", swiss(), 201).await;
    // 20.00 EUR at 0.95 is 19.00 CHF, shown as 19.99.
    let r = send(&client, &t, "acme", "GET", &price, Value::Null, 200).await;
    assert_eq!(r["price_minor"], 1999);
    assert_eq!(r["currency"], "CHF");
    assert_eq!(r["market_id"], m["id"]);
    // A price list in the currency wins.
    let list = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/price-lists",
        json!({ "name": "Swiss", "currency": "CHF" }),
        201,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("/api/price-lists/{}/prices", list["id"].as_str().unwrap()),
        json!({ "prices": [{ "item_id": id, "price_minor": 2490 }] }),
        200,
    )
    .await;
    let r = send(&client, &t, "acme", "GET", &price, Value::Null, 200).await;
    assert_eq!(r["price_minor"], 2490);
    assert_eq!(r["market_id"], Value::Null);
    // The own currency is untouched.
    let r = send(
        &client,
        &t,
        "acme",
        "GET",
        &format!("/api/prices/{id}?currency=EUR"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(r["price_minor"], 2000);
}

fn cookie_of(res: &zapline::test::TestResponse, name: &str) -> String {
    let c = res.cookie(name).unwrap();
    format!("{name}={}", c.value())
}

#[tokio::test]
async fn buyers_pick_their_country() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let p = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/products",
        json!({ "sku": "SHOE", "title": "Trail Shoe", "price_minor": 2000, "currency": "EUR" }),
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
    let get = |path: &'static str, cookie: String| {
        let client = &client;
        async move {
            let res = client
                .get(path)
                .header("x-soat-tenant", "acme")
                .header("cookie", &cookie)
                .send()
                .await;
            res.assert_status(200);
            res.text()
        }
    };

    // No markets: no picker, prices in the product's currency.
    let html = get("/products/trail-shoe", String::new()).await;
    assert!(html.contains("20.00 EUR") && !html.contains("name=\"country\""));

    send(&client, &t, "acme", "POST", "/api/markets", swiss(), 201).await;
    let html = get("/products/trail-shoe", String::new()).await;
    assert!(
        html.contains("<option value=\"CH\">Switzerland (CHF)</option>"),
        "{html}"
    );
    assert!(html.contains("20.00 EUR"));

    // A cart started before picking a country moves to the market's
    // currency.
    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .form(&json!({ "product_id": id }))
        .send()
        .await;
    res.assert_status(303);
    let cart = cookie_of(&res, "soat_cart");
    let res = client
        .post("/de/country")
        .header("x-soat-tenant", "acme")
        .header("cookie", &cart)
        .form(&json!({ "country": "ch", "back": "/de/products/trail-shoe" }))
        .send()
        .await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some("/de/products/trail-shoe"));
    let country = cookie_of(&res, "soat_country");
    assert_eq!(country, "soat_country=CH");
    let both = format!("{cart}; {country}");

    let html = get("/products/trail-shoe", both.clone()).await;
    assert!(html.contains("19.99 CHF"), "{html}");
    assert!(html.contains("<option value=\"CH\" selected>"));
    let html = get("/", both.clone()).await;
    assert!(html.contains("19.99 CHF"));
    let html = get("/cart", both.clone()).await;
    assert!(html.contains("19.99 CHF"), "{html}");

    // "Other" goes back to the shop's own currency; outside addresses are
    // never a target.
    let res = client
        .post("/country")
        .header("x-soat-tenant", "acme")
        .header("cookie", &both)
        .form(&json!({ "country": "", "back": "//evil.example" }))
        .send()
        .await;
    assert_eq!(res.header("location"), Some("/"));
    assert!(res.cookie("soat_country").unwrap().is_removal());
    let html = get("/cart", cart.clone()).await;
    assert!(html.contains("20.00 EUR"), "{html}");

    // A new cart starts in the market's currency.
    let res = client
        .post("/cart/add")
        .header("x-soat-tenant", "acme")
        .header("cookie", "soat_country=LI")
        .form(&json!({ "product_id": id }))
        .send()
        .await;
    res.assert_status(303);
    let cart = cookie_of(&res, "soat_cart");
    let html = get("/cart", cart).await;
    assert!(html.contains("19.99 CHF"), "{html}");
}
