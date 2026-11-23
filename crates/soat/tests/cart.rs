//! C3: carts on the storefront, and staff's view of them.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

/// A request as staff (`bearer`) or as a shopper holding `cart` tokens.
#[allow(clippy::too_many_arguments)]
async fn call(
    client: &TestClient,
    shop: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    cart: Option<&str>,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        "DELETE" => client.delete(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match bearer {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = match cart {
        Some(t) => req.header("x-soat-cart-token", t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status == 204 || status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

/// A EUR product with `units` on hand; returns its id.
async fn stocked(client: &TestClient, token: &str, sku: &str, price: i64, units: i64) -> String {
    let admin = Some(token);
    let locs = call(
        client,
        "acme",
        "GET",
        "/api/locations",
        admin,
        None,
        Value::Null,
        200,
    )
    .await;
    let loc = match locs["items"].as_array().and_then(|l| l.first()) {
        Some(l) => l["id"].clone(),
        None => call(
            client,
            "acme",
            "POST",
            "/api/locations",
            admin,
            None,
            json!({ "name": "W", "country": "DE" }),
            201,
        )
        .await["id"]
            .clone(),
    };
    let p = call(
        client,
        "acme",
        "POST",
        "/api/products",
        admin,
        None,
        json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR" }),
        201,
    )
    .await;
    let id = p["id"].as_str().unwrap().to_owned();
    call(
        client,
        "acme",
        "POST",
        &format!("/api/inventory/{id}/adjustments"),
        admin,
        None,
        json!({ "location_id": loc, "delta": units, "reason": "received" }),
        200,
    )
    .await;
    id
}

async fn new_cart(client: &TestClient, shop: &str, currency: &str) -> (String, String) {
    let c = call(
        client,
        shop,
        "POST",
        "/api/storefront/carts",
        None,
        None,
        json!({ "currency": currency }),
        201,
    )
    .await;
    let token = c["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("soat_cart_"));
    assert!(c["cart"]["id"].as_str().unwrap().starts_with("cart_"));
    assert_eq!(c["cart"]["ready"], false);
    (c["cart"]["id"].as_str().unwrap().to_owned(), token)
}

#[tokio::test]
async fn shoppers_fill_a_cart_with_live_prices_and_stock() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let tee = stocked(&client, &admin, "TEE", 1500, 5).await;
    let cap = stocked(&client, &admin, "CAP", 900, 1).await;
    let (id, token) = new_cart(&client, "acme", "EUR").await;
    let path = format!("/api/storefront/carts/{id}");
    let lines = format!("{path}/lines");
    let t = Some(token.as_str());

    let add = |item: &str, quantity: i64| json!({ "item_id": item, "quantity": quantity });
    call(&client, "acme", "POST", &lines, None, t, add(&tee, 2), 200).await;
    let cart = call(&client, "acme", "POST", &lines, None, t, add(&tee, 1), 200).await;
    assert_eq!(
        cart["lines"].as_array().unwrap().len(),
        1,
        "same item merges"
    );
    assert_eq!(cart["lines"][0]["quantity"], 3);
    assert_eq!(cart["lines"][0]["unit_price_minor"], 1500);
    assert_eq!(cart["subtotal_minor"], 4500);
    assert_eq!(cart["ready"], true);

    call(&client, "acme", "POST", &lines, None, t, add(&tee, 3), 409).await;
    call(
        &client,
        "acme",
        "POST",
        &lines,
        None,
        t,
        add("prod_nope", 1),
        422,
    )
    .await;
    let cart = call(&client, "acme", "POST", &lines, None, t, add(&cap, 1), 200).await;
    assert_eq!(cart["item_count"], 4);
    assert_eq!(cart["subtotal_minor"], 5400);

    // Stock sold elsewhere flags the line instead of hiding it.
    call(
        &client,
        "acme",
        "POST",
        "/api/inventory/reservations",
        Some(&admin),
        None,
        json!({ "item_id": cap, "quantity": 1, "reference": "elsewhere" }),
        201,
    )
    .await;
    let cart = call(&client, "acme", "GET", &path, None, t, Value::Null, 200).await;
    let cap_line = cart["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["item_id"] == cap.as_str())
        .unwrap()
        .clone();
    assert_eq!(cap_line["issue"], "insufficient_stock");
    assert_eq!(cart["ready"], false);

    // Change, then remove lines.
    let line = cap_line["id"].as_str().unwrap();
    let tee_line = cart["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["item_id"] == tee.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cart = call(
        &client,
        "acme",
        "PATCH",
        &format!("{lines}/{tee_line}"),
        None,
        t,
        json!({ "quantity": 1 }),
        200,
    )
    .await;
    assert_eq!(cart["subtotal_minor"], 2400);
    call(
        &client,
        "acme",
        "PATCH",
        &format!("{lines}/{tee_line}"),
        None,
        t,
        json!({ "quantity": 6 }),
        409,
    )
    .await;
    let cart = call(
        &client,
        "acme",
        "DELETE",
        &format!("{lines}/{line}"),
        None,
        t,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(cart["ready"], true);
    let cart = call(
        &client,
        "acme",
        "PATCH",
        &format!("{lines}/{tee_line}"),
        None,
        t,
        json!({ "quantity": 0 }),
        200,
    )
    .await;
    assert!(cart["lines"].as_array().unwrap().is_empty());
    call(
        &client,
        "acme",
        "DELETE",
        &format!("{lines}/{line}"),
        None,
        t,
        Value::Null,
        404,
    )
    .await;

    let cart = call(
        &client,
        "acme",
        "PATCH",
        &path,
        None,
        t,
        json!({ "email": "Ada@Example.com" }),
        200,
    )
    .await;
    assert_eq!(cart["email"], "ada@example.com");
}

#[tokio::test]
async fn carts_need_their_token_and_a_price_in_their_currency() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let tee = stocked(&client, &admin, "TEE", 1500, 5).await;
    let (id, token) = new_cart(&client, "acme", "USD").await;
    let path = format!("/api/storefront/carts/{id}");
    call(&client, "acme", "GET", &path, None, None, Value::Null, 401).await;
    call(
        &client,
        "acme",
        "GET",
        &path,
        None,
        Some("soat_cart_wrong"),
        Value::Null,
        404,
    )
    .await;
    let (_, other) = new_cart(&client, "acme", "EUR").await;
    call(
        &client,
        "acme",
        "GET",
        &path,
        None,
        Some(&other),
        Value::Null,
        404,
    )
    .await;
    call(
        &client,
        "acme",
        "POST",
        &format!("{path}/lines"),
        None,
        Some(&token),
        json!({ "item_id": tee, "quantity": 1 }),
        422,
    )
    .await;
    call(
        &client,
        "acme",
        "POST",
        "/api/storefront/carts",
        None,
        None,
        json!({ "currency": "euro" }),
        422,
    )
    .await;
}

#[tokio::test]
async fn signed_in_customers_claim_their_cart_and_staff_see_it() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
    let (id, token) = new_cart(&client, "acme", "EUR").await;
    let s = call(
        &client,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        None,
        json!({ "email": "ada@example.com", "password": "a long password" }),
        201,
    )
    .await;
    let customer = s["token"].as_str().unwrap();
    let path = format!("/api/storefront/carts/{id}/customer");
    call(
        &client,
        "acme",
        "POST",
        &path,
        Some(customer),
        None,
        Value::Null,
        401,
    )
    .await;
    let cart = call(
        &client,
        "acme",
        "POST",
        &path,
        Some(customer),
        Some(&token),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(cart["customer_id"], s["customer"]["id"]);
    assert_eq!(cart["email"], "ada@example.com");

    let page = call(
        &client,
        "acme",
        "GET",
        "/api/carts?q=ada",
        Some(&admin),
        None,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], id.as_str());
    let one = call(
        &client,
        "acme",
        "GET",
        &format!("/api/carts/{id}"),
        Some(&admin),
        None,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(one["status"], "open");
}

#[tokio::test]
async fn another_shops_cart_is_not_found() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let (id, token) = new_cart(&client, "acme", "EUR").await;
    let path = format!("/api/storefront/carts/{id}");
    call(
        &client,
        "globex",
        "GET",
        &path,
        None,
        Some(&token),
        Value::Null,
        404,
    )
    .await;
    call(
        &client,
        "globex",
        "GET",
        &format!("/api/carts/{id}"),
        Some(&admin),
        None,
        Value::Null,
        404,
    )
    .await;
    let page = call(
        &client,
        "globex",
        "GET",
        "/api/carts",
        Some(&admin),
        None,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 0);
}
