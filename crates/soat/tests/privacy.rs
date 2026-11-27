//! J1: GDPR. A customer's data exported and erased across every module,
//! their requests, and a guard that no table with customer data is left
//! out of the declarations.
#![cfg(feature = "privacy")]

mod common;

use serde_json::{json, Value};
use zapline::db::sqlx::{self, Connection, PgConnection};
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

/// Tables that hold people's data but not a customer's.
const NOT_CUSTOMERS: &[(&str, &str)] = &[
    ("users", "staff accounts"),
    ("invoice_settings", "the shop's own address"),
    (
        "privacy_requests",
        "names a customer by id only, to outlive them",
    ),
];

#[tokio::test]
async fn every_table_with_customer_data_is_declared() {
    let (client, url) = common::client_and_url().await;
    // The first request runs the migrations.
    common::setup_admin(&client).await;
    let mut conn = PgConnection::connect(&url).await.unwrap();
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT table_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND column_name IN \
         ('customer_id', 'email', 'recipient', 'recipient_email', 'phone', 'author_name') \
         ORDER BY 1",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert!(tables.len() > 10, "{tables:?}");
    let map = soat::modules(soat::Settings::for_tests("t")).personal_data();
    let missing: Vec<&str> = tables
        .iter()
        .map(|(t,)| t.as_str())
        .filter(|t| !map.covers(t) && !NOT_CUSTOMERS.iter().any(|(n, _)| n == t))
        .collect();
    assert!(
        missing.is_empty(),
        "declare these in their module's personal_data(): {missing:?}"
    );
    // Every declaration names a real table and real columns.
    for (module, d) in map.all() {
        let cols: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name::text, is_nullable::text FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1",
        )
        .bind(d.table)
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert!(!cols.is_empty(), "{module}: no table {}", d.table);
        let has = |c: &str| cols.iter().any(|(x, _)| x == c);
        for c in d.export {
            assert!(has(c), "{module}: {} has no column {c}", d.table);
        }
        if let soat_module::Erase::Anonymize(set) = d.erase {
            for (c, v) in set {
                assert!(has(c), "{module}: {} has no column {c}", d.table);
                let nullable = cols.iter().any(|(x, n)| x == c && n == "YES");
                assert!(
                    *v != "NULL" || nullable,
                    "{module}: {}.{c} is NOT NULL",
                    d.table
                );
            }
        }
    }
}

#[tokio::test]
async fn customers_get_their_data_and_are_erased() {
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
    let mug = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": "MUG", "title": "Mug", "price_minor": 1200, "currency": "EUR" }),
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
        &format!("/api/inventory/{mug}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 5, "reason": "received" }),
        200,
    )
    .await;

    // Ann registers, saves an address and buys a mug.
    let email = "ann.privacy@example.com";
    let token = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": email, "password": "a long password", "first_name": "Ann" }),
        201,
    )
    .await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let t = Some(token.as_str());
    let ann = call(
        c,
        "acme",
        "GET",
        "/api/customers?q=ann.privacy",
        a,
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let address = json!({ "first_name": "Ann", "last_name": "Example", "line1": "Hauptstr. 1",
                          "postal_code": "10115", "city": "Berlin", "country": "DE" });
    call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ann}/addresses"),
        a,
        address.clone(),
        201,
    )
    .await;
    let draft = call(
        c,
        "acme",
        "POST",
        "/api/draft-orders",
        a,
        json!({ "customer_id": ann, "email": email, "currency": "EUR",
                "lines": [{ "item_id": mug, "quantity": 1 }],
                "shipping_address": address, "billing_address": address }),
        201,
    )
    .await;
    let order = call(
        c,
        "acme",
        "POST",
        &format!(
            "/api/draft-orders/{}/complete",
            draft["id"].as_str().unwrap()
        ),
        a,
        json!({}),
        201,
    )
    .await;
    let order_id = order["id"]
        .as_str()
        .or(order["order"]["id"].as_str())
        .unwrap()
        .to_owned();

    // Staff and Ann herself get everything, but no session tokens.
    let export = call(
        c,
        "acme",
        "GET",
        &format!("/api/customers/{ann}/personal-data"),
        a,
        Value::Null,
        200,
    )
    .await;
    let tables = &export["tables"];
    assert_eq!(tables["customers"][0]["email"], email, "{export}");
    assert_eq!(tables["customer_addresses"].as_array().unwrap().len(), 1);
    assert_eq!(tables["orders"][0]["id"], order_id.as_str());
    assert!(tables.get("customer_sessions").is_none());
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/personal-data",
        t,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine["tables"]["orders"][0]["id"], order_id.as_str());
    call(
        c,
        "other",
        "GET",
        &format!("/api/customers/{ann}/personal-data"),
        a,
        Value::Null,
        404,
    )
    .await;

    // Ann asks to be erased; asking again gives the same request.
    let asked = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/erasure-request",
        t,
        Value::Null,
        201,
    )
    .await;
    let again = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/erasure-request",
        t,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(asked["id"], again["id"]);
    let open = call(
        c,
        "acme",
        "GET",
        "/api/privacy-requests?status=open",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(open["items"][0]["email"], email);
    let none = call(
        c,
        "other",
        "GET",
        "/api/privacy-requests",
        a,
        Value::Null,
        200,
    )
    .await;
    assert!(none["items"].as_array().unwrap().is_empty());

    // A key that may only read cannot erase; a wrong email does not.
    let key = c
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "name": "reader", "permissions": ["privacy:read", "privacy:export"] }))
        .send()
        .await
        .assert_status(201)
        .json::<Value>()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ann}/erase"),
        Some(&key),
        json!({ "email": email }),
        403,
    )
    .await;
    let wrong = call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ann}/erase"),
        a,
        json!({ "email": "someone@example.com" }),
        422,
    )
    .await;
    assert_eq!(wrong["code"], "email_mismatch");
    call(
        c,
        "other",
        "POST",
        &format!("/api/customers/{ann}/erase"),
        a,
        json!({ "email": email }),
        404,
    )
    .await;

    let report = call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ann}/erase"),
        a,
        json!({ "email": email.to_uppercase() }),
        200,
    )
    .await;
    let row = |table: &str| {
        report["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["table"] == table)
            .cloned()
            .unwrap_or_else(|| panic!("{table} in {report}"))
    };
    assert_eq!(row("customers")["action"], "deleted");
    assert_eq!(row("customers")["rows"], 1);
    assert_eq!(row("customer_addresses")["rows"], 1);
    assert!(row("customer_sessions")["rows"].as_i64().unwrap() >= 1);
    assert_eq!(row("orders")["action"], "anonymized");
    assert_eq!(row("orders")["rows"], 1);
    assert_eq!(row("invoices")["action"], "kept");
    assert!(row("invoices")["reason"].as_str().unwrap().contains("tax"));
    assert!(report["events_redacted"].as_i64().unwrap() >= 1, "{report}");

    // Gone: the customer, her session; the order stays without her.
    call(
        c,
        "acme",
        "GET",
        &format!("/api/customers/{ann}"),
        a,
        Value::Null,
        404,
    )
    .await;
    call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/personal-data",
        t,
        Value::Null,
        401,
    )
    .await;
    let kept = call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{order_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(kept["email"], "erased@invalid");
    assert!(kept["customer_id"].is_null());
    assert_eq!(kept["billing_address"]["city"], "Berlin");
    let log = call(
        c,
        "acme",
        "GET",
        "/api/events?limit=200",
        a,
        Value::Null,
        200,
    )
    .await;
    assert!(
        !log.to_string().to_lowercase().contains(email),
        "the event log still names her"
    );
    let erased = call(
        c,
        "acme",
        "GET",
        "/api/events?kind=customer.erased",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(erased["items"][0]["payload"]["id"], ann.as_str());
    let done = call(
        c,
        "acme",
        "GET",
        "/api/privacy-requests",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(done["items"][0]["status"], "done");
    assert!(done["items"][0]["email"].is_null());

    // Bob asks too; staff decline, once.
    let bob = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": "bob.privacy@example.com", "password": "a long password" }),
        201,
    )
    .await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let asked = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/erasure-request",
        Some(&bob),
        Value::Null,
        201,
    )
    .await;
    let rid = asked["id"].as_str().unwrap().to_owned();
    call(
        c,
        "other",
        "POST",
        &format!("/api/privacy-requests/{rid}/decline"),
        a,
        json!({ "note": "x" }),
        404,
    )
    .await;
    let declined = call(
        c,
        "acme",
        "POST",
        &format!("/api/privacy-requests/{rid}/decline"),
        a,
        json!({ "note": "An unpaid invoice is still owed." }),
        200,
    )
    .await;
    assert_eq!(declined["status"], "declined");
    call(
        c,
        "acme",
        "POST",
        &format!("/api/privacy-requests/{rid}/decline"),
        a,
        json!({ "note": "again" }),
        409,
    )
    .await;

    // The admin: the customer page, the download, the requests page.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let bob_id = call(
        c,
        "acme",
        "GET",
        "/api/customers?q=bob.privacy",
        a,
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let html = c
        .get(&format!("/admin/acme/customers/{bob_id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Erase customer"), "{html}");
    let res = c
        .get(&format!("/admin/acme/customers/{bob_id}/personal-data"))
        .header("cookie", &cookie)
        .send()
        .await;
    res.assert_status(200);
    assert!(res
        .header("content-disposition")
        .unwrap()
        .starts_with("attachment"));
    assert!(res.text().contains("bob.privacy@example.com"));
    let html = c
        .post(&format!("/admin/acme/customers/{bob_id}/erase"))
        .header("cookie", &cookie)
        .form(&json!({ "email": "not-bob@example.com" }))
        .send()
        .await
        .text();
    assert!(html.contains("Not erased"), "{html}");
    let html = c
        .post(&format!("/admin/acme/customers/{bob_id}/erase"))
        .header("cookie", &cookie)
        .form(&json!({ "email": "bob.privacy@example.com" }))
        .send()
        .await
        .text();
    assert!(
        html.contains("The customer is erased") && html.contains("declined"),
        "{html}"
    );

    // The storefront account page offers both.
    let res = c
        .post("/api/storefront/account/register")
        .header("x-soat-tenant", "acme")
        .json(&json!({ "email": "cy@example.com", "password": "a long password" }))
        .send()
        .await;
    let cy: Value = res.json();
    let html = c
        .get("/account")
        .header("x-soat-tenant", "acme")
        .header(
            "cookie",
            &format!("soat_customer={}", cy["token"].as_str().unwrap()),
        )
        .send()
        .await
        .text();
    assert!(html.contains("/account/personal-data"), "{html}");
}
