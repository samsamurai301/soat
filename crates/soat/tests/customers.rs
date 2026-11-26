//! C1: customers.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn send(
    client: &TestClient,
    token: &str,
    shop: &str,
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

fn address(city: &str, default: bool) -> Value {
    json!({ "line1": "Hauptstr. 1", "city": city, "postal_code": "10115", "country": "DE", "is_default": default })
}

#[tokio::test]
async fn customers_have_profiles_consent_and_addresses() {
    let (client, t) = common::with_tenants(&["acme"]).await;
    let c = send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "Ada@Example.com", "first_name": "Ada", "tags": ["vip"] }),
        201,
    )
    .await;
    assert_eq!(c["email"], "ada@example.com");
    assert!(c["id"].as_str().unwrap().starts_with("cus_"));
    assert!(c["marketing_consent_at"].is_null());
    send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "ADA@example.com" }),
        409,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "not-an-email" }),
        422,
    )
    .await;
    let id = c["id"].as_str().unwrap();

    // Consent keeps its first time until withdrawn.
    let on = send(
        &client,
        &t,
        "acme",
        "PATCH",
        &format!("/api/customers/{id}"),
        json!({ "accepts_marketing": true }),
        200,
    )
    .await;
    let at = on["marketing_consent_at"].as_str().unwrap().to_owned();
    let again = send(
        &client,
        &t,
        "acme",
        "PATCH",
        &format!("/api/customers/{id}"),
        json!({ "accepts_marketing": true, "last_name": "Lovelace" }),
        200,
    )
    .await;
    assert_eq!(again["marketing_consent_at"], at.as_str());
    assert_eq!(again["first_name"], "Ada");
    let off = send(
        &client,
        &t,
        "acme",
        "PATCH",
        &format!("/api/customers/{id}"),
        json!({ "accepts_marketing": false }),
        200,
    )
    .await;
    assert!(off["marketing_consent_at"].is_null());

    let page = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/customers?q=lovelace",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 1);
    let page = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/customers?tag=vip",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 1);

    // The first address is the default; a new default takes over.
    let path = format!("/api/customers/{id}/addresses");
    let berlin = send(
        &client,
        &t,
        "acme",
        "POST",
        &path,
        address("Berlin", false),
        201,
    )
    .await;
    assert_eq!(berlin["is_default"], true);
    let munich = send(
        &client,
        &t,
        "acme",
        "POST",
        &path,
        address("Munich", true),
        201,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "POST",
        &path,
        json!({ "line1": "x", "city": "y", "country": "de" }),
        422,
    )
    .await;
    let list = send(&client, &t, "acme", "GET", &path, Value::Null, 200).await;
    assert_eq!(list[0]["city"], "Munich");
    assert_eq!(list[1]["is_default"], false);
    // Deleting the default promotes another.
    send(
        &client,
        &t,
        "acme",
        "DELETE",
        &format!("{path}/{}", munich["id"].as_str().unwrap()),
        Value::Null,
        204,
    )
    .await;
    let list = send(&client, &t, "acme", "GET", &path, Value::Null, 200).await;
    assert_eq!(
        (list[0]["city"].as_str(), list[0]["is_default"].as_bool()),
        (Some("Berlin"), Some(true))
    );
    let replaced = send(
        &client,
        &t,
        "acme",
        "PUT",
        &format!("{path}/{}", berlin["id"].as_str().unwrap()),
        address("Potsdam", false),
        200,
    )
    .await;
    assert_eq!(
        (replaced["city"].as_str(), replaced["is_default"].as_bool()),
        (Some("Potsdam"), Some(true))
    );

    // Custom fields work for customers too.
    send(
        &client,
        &t,
        "acme",
        "PUT",
        "/api/fields/customer/birthday",
        json!({ "name": "Birthday", "kind": "date" }),
        200,
    )
    .await;
    let f = send(
        &client,
        &t,
        "acme",
        "PATCH",
        &format!("/api/customers/{id}/fields"),
        json!({ "birthday": "1815-12-10" }),
        200,
    )
    .await;
    assert_eq!(f["birthday"], "1815-12-10");

    send(
        &client,
        &t,
        "acme",
        "DELETE",
        &format!("/api/customers/{id}"),
        Value::Null,
        204,
    )
    .await;
    send(
        &client,
        &t,
        "acme",
        "GET",
        &format!("/api/customers/{id}"),
        Value::Null,
        404,
    )
    .await;
}

#[tokio::test]
async fn customers_stay_in_their_shop() {
    let (client, t) = common::with_tenants(&["acme", "globex"]).await;
    let theirs = send(
        &client,
        &t,
        "globex",
        "POST",
        "/api/customers",
        json!({ "email": "g@example.com" }),
        201,
    )
    .await;
    let theirs = theirs["id"].as_str().unwrap();
    let addr = send(
        &client,
        &t,
        "globex",
        "POST",
        &format!("/api/customers/{theirs}/addresses"),
        address("Bonn", false),
        201,
    )
    .await;
    let addr = addr["id"].as_str().unwrap();
    // The same email is a different customer in another shop.
    send(
        &client,
        &t,
        "acme",
        "POST",
        "/api/customers",
        json!({ "email": "g@example.com" }),
        201,
    )
    .await;

    for (method, path) in [
        ("GET", format!("/api/customers/{theirs}")),
        ("DELETE", format!("/api/customers/{theirs}")),
        ("GET", format!("/api/customers/{theirs}/addresses")),
        (
            "DELETE",
            format!("/api/customers/{theirs}/addresses/{addr}"),
        ),
        ("GET", format!("/api/customers/{theirs}/fields")),
    ] {
        send(&client, &t, "acme", method, &path, Value::Null, 404).await;
    }
    let page = send(
        &client,
        &t,
        "acme",
        "GET",
        "/api/customers",
        Value::Null,
        200,
    )
    .await;
    assert_eq!(page["total"], 1);
}
