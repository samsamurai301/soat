//! C10: orders, placed through the library as checkout will, followed by
//! their payments.

mod common;

use serde_json::{json, Value};
use soat_core::Currency;
use soat_orders::{place, NewOrder, NewOrderLine, NewShipping, PostalAddress};
use soat_tenancy::Scope;
use zapline::db::sqlx::{self, PgPool};
use zapline::test::TestClient;

struct In(&'static str);

impl Scope for In {
    fn tenant_id(&self) -> &str {
        self.0
    }
    fn actor_id(&self) -> Option<&str> {
        None
    }
}

fn order(customer: Option<&str>) -> NewOrder {
    NewOrder {
        cart_id: None,
        customer_id: customer.map(str::to_owned),
        email: "Ada@Example.com".into(),
        currency: Currency::parse("EUR").unwrap(),
        lines: vec![
            NewOrderLine {
                item_id: "prod_a".into(),
                product_id: Some("prod_a".into()),
                sku: "TEE".into(),
                title: "Tee".into(),
                quantity: 2,
                unit_price_minor: 1000,
                tax_class: "standard".into(),
                tax_rate_bps: 1900,
                tax_minor: 380,
            },
            NewOrderLine {
                item_id: "prod_b".into(),
                product_id: Some("prod_b".into()),
                sku: "BOOK".into(),
                title: "Book".into(),
                quantity: 1,
                unit_price_minor: 1500,
                tax_class: "reduced".into(),
                tax_rate_bps: 700,
                tax_minor: 105,
            },
        ],
        shipping: Some(NewShipping {
            rate_id: "shr_x".into(),
            name: "Standard".into(),
            price_minor: 500,
            tax_minor: 95,
        }),
        shipping_address: Some(PostalAddress {
            first_name: "Ada".into(),
            line1: "Hauptstr. 1".into(),
            postal_code: "10115".into(),
            city: "Berlin".into(),
            country: "DE".into(),
            ..PostalAddress::default()
        }),
        billing_address: None,
        prices_include_tax: false,
        tax_country: Some("DE".into()),
        reverse_charge: false,
        vat_number: None,
        note: String::new(),
    }
}

/// Places an order for `shop` straight through the library.
async fn placed(url: &str, shop: &'static str, customer: Option<&str>) -> (String, String) {
    let pool = PgPool::connect(url).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', $1, true)")
        .bind(shop)
        .execute(&mut *tx)
        .await
        .unwrap();
    let (o, token) = place(&mut tx, &In(shop), &order(customer)).await.unwrap();
    tx.commit().await.unwrap();
    (o.id, token)
}

async fn send(
    client: &TestClient,
    shop: &str,
    auth: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    let req = match method {
        "POST" => client.post(path),
        "PATCH" => client.patch(path),
        _ => client.get(path),
    }
    .header("x-soat-tenant", shop);
    let req = match auth {
        Some(t) => req.bearer(t),
        None => req,
    };
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    let res = res.assert_status(status);
    if status >= 400 {
        Value::Null
    } else {
        res.json()
    }
}

async fn dispatch(client: &TestClient) {
    soat_events::dispatch_resources(&client.resources().await)
        .await
        .unwrap();
}

async fn setup(shops: &[&str]) -> (TestClient, String, String) {
    let (client, url) = common::client_and_url().await;
    let token = common::setup_admin(&client).await;
    common::create_tenants(&client, &token, shops).await;
    (client, token, url)
}

#[tokio::test]
async fn orders_are_numbered_and_follow_their_payments() {
    let (client, t, url) = setup(&["acme"]).await;
    let (id, _) = placed(&url, "acme", None).await;
    let (second, _) = placed(&url, "acme", None).await;
    let o = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        &format!("/api/orders/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["number"], 1001);
    assert_eq!(o["email"], "ada@example.com");
    assert_eq!(o["subtotal_minor"], 3500);
    assert_eq!(o["tax_minor"], 580);
    assert_eq!(o["total_minor"], 3500 + 500 + 580);
    assert_eq!(o["lines"][0]["total_minor"], 2000);
    assert_eq!(o["shipping_address"]["city"], "Berlin");
    assert_eq!(o["financial_status"], "pending");
    let list = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        "/api/orders",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["items"][0]["id"], second.as_str());
    assert_eq!(list["items"][0]["number"], 1002);

    let m = send(
        &client,
        "acme",
        Some(&t),
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice" }),
        201,
    )
    .await;
    let p = send(
        &client,
        "acme",
        Some(&t),
        "POST",
        "/api/payments",
        json!({ "method_id": m["id"], "reference": id, "amount_minor": 4580, "currency": "EUR" }),
        201,
    )
    .await;
    let pay = p["id"].as_str().unwrap();
    send(
        &client,
        "acme",
        Some(&t),
        "POST",
        &format!("/api/payments/{pay}/mark-paid"),
        Value::Null,
        200,
    )
    .await;
    dispatch(&client).await;
    let o = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        &format!("/api/orders/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["financial_status"], "paid");
    let events = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        "/api/events?kind=order.paid",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(events["total"], 1);

    send(
        &client,
        "acme",
        Some(&t),
        "POST",
        &format!("/api/payments/{pay}/refunds"),
        json!({ "amount_minor": 1000 }),
        201,
    )
    .await;
    dispatch(&client).await;
    dispatch(&client).await;
    let o = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        &format!("/api/orders/{id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["financial_status"], "partially_refunded");
    let o = send(
        &client,
        "acme",
        Some(&t),
        "PATCH",
        &format!("/api/orders/{id}"),
        json!({ "note": "gift wrap" }),
        200,
    )
    .await;
    assert_eq!(o["note"], "gift wrap");
}

#[tokio::test]
async fn canceling_releases_held_stock() {
    let (client, t, url) = setup(&["acme"]).await;
    let (id, _) = placed(&url, "acme", None).await;
    let p = send(
        &client,
        "acme",
        Some(&t),
        "POST",
        "/api/products",
        json!({ "sku": "TEE", "title": "Tee", "price_minor": 1000, "currency": "EUR" }),
        201,
    )
    .await;
    let item = p["id"].as_str().unwrap();
    // Untracked items can always be held.
    send(
        &client,
        "acme",
        Some(&t),
        "PATCH",
        &format!("/api/inventory/{item}"),
        json!({ "tracked": false }),
        200,
    )
    .await;
    send(
        &client,
        "acme",
        Some(&t),
        "POST",
        "/api/inventory/reservations",
        json!({ "item_id": item, "quantity": 2, "reference": id }),
        201,
    )
    .await;
    let o = send(
        &client,
        "acme",
        Some(&t),
        "POST",
        &format!("/api/orders/{id}/cancel"),
        json!({ "reason": "customer asked" }),
        200,
    )
    .await;
    assert_eq!(o["status"], "canceled");
    assert_eq!(o["cancel_reason"], "customer asked");
    let held = send(
        &client,
        "acme",
        Some(&t),
        "GET",
        &format!("/api/inventory/reservations?reference={id}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(held["total"], 0);
    send(
        &client,
        "acme",
        Some(&t),
        "POST",
        &format!("/api/orders/{id}/cancel"),
        json!({}),
        409,
    )
    .await;
}

#[tokio::test]
async fn buyers_see_their_own_orders_only() {
    let (client, _, url) = setup(&["acme"]).await;
    let s = send(
        &client,
        "acme",
        None,
        "POST",
        "/api/storefront/account/register",
        json!({ "email": "ada@example.com", "password": "a long password" }),
        201,
    )
    .await;
    let customer = s["token"].as_str().unwrap();
    let cus = s["customer"]["id"].as_str().unwrap();
    let (mine, _) = placed(&url, "acme", Some(cus)).await;
    let (guest, token) = placed(&url, "acme", None).await;

    let list = send(
        &client,
        "acme",
        Some(customer),
        "GET",
        "/api/storefront/account/orders",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], mine.as_str());
    send(
        &client,
        "acme",
        Some(customer),
        "GET",
        &format!("/api/storefront/account/orders/{mine}"),
        Value::Null,
        200,
    )
    .await;
    send(
        &client,
        "acme",
        Some(customer),
        "GET",
        &format!("/api/storefront/account/orders/{guest}"),
        Value::Null,
        404,
    )
    .await;

    let path = format!("/api/storefront/orders/{guest}");
    let o: Value = client
        .get(&path)
        .header("x-soat-tenant", "acme")
        .header("x-soat-order-token", &token)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(o["number"], 1002);
    client
        .get(&path)
        .header("x-soat-tenant", "acme")
        .header("x-soat-order-token", "soat_order_wrong")
        .send()
        .await
        .assert_status(404);
    client
        .get(&path)
        .header("x-soat-tenant", "acme")
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn another_shops_orders_are_not_found() {
    let (client, t, url) = setup(&["acme", "globex"]).await;
    let (id, token) = placed(&url, "acme", None).await;
    send(
        &client,
        "globex",
        Some(&t),
        "GET",
        &format!("/api/orders/{id}"),
        Value::Null,
        404,
    )
    .await;
    send(
        &client,
        "globex",
        Some(&t),
        "PATCH",
        &format!("/api/orders/{id}"),
        json!({ "note": "x" }),
        404,
    )
    .await;
    send(
        &client,
        "globex",
        Some(&t),
        "POST",
        &format!("/api/orders/{id}/cancel"),
        json!({}),
        404,
    )
    .await;
    let list = send(
        &client,
        "globex",
        Some(&t),
        "GET",
        "/api/orders",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(list["total"], 0);
    client
        .get(&format!("/api/storefront/orders/{id}"))
        .header("x-soat-tenant", "globex")
        .header("x-soat-order-token", &token)
        .send()
        .await
        .assert_status(404);
    // Numbers are per shop.
    let (other, _) = placed(&url, "globex", None).await;
    let o = send(
        &client,
        "globex",
        Some(&t),
        "GET",
        &format!("/api/orders/{other}"),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(o["number"], 1001);
}
