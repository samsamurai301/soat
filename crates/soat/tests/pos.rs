//! I11: point of sale. Registers at a location, sessions with a cash
//! count, sales by SKU paid in cash or card, stock taken at the register.
#![cfg(feature = "pos")]

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

fn on_hand(inventory: &Value, location: &Value) -> i64 {
    inventory["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["location_id"] == *location)
        .map_or(0, |l| l["on_hand"].as_i64().unwrap())
}

#[tokio::test]
async fn pos_sells_at_the_counter_and_counts_the_cash() {
    let (client, admin) = common::with_tenants(&["acme", "other"]).await;
    let c = &client;
    let a = Some(admin.as_str());
    let store = call(
        c,
        "acme",
        "POST",
        "/api/locations",
        a,
        json!({ "name": "Berlin store", "country": "DE" }),
        201,
    )
    .await["id"]
        .clone();
    let warehouse = call(
        c,
        "acme",
        "POST",
        "/api/locations",
        a,
        json!({ "name": "Warehouse", "country": "DE" }),
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
    for (loc, n) in [(&store, 5), (&warehouse, 10)] {
        call(
            c,
            "acme",
            "POST",
            &format!("/api/inventory/{mug}/adjustments"),
            a,
            json!({ "location_id": loc, "delta": n, "reason": "received" }),
            200,
        )
        .await;
    }

    // A register at the store; one session at a time.
    call(
        c,
        "acme",
        "POST",
        "/api/pos/registers",
        a,
        json!({ "name": "Desk", "location_id": "loc_nope" }),
        422,
    )
    .await;
    let reg = call(
        c,
        "acme",
        "POST",
        "/api/pos/registers",
        a,
        json!({ "name": "Front desk", "location_id": store }),
        201,
    )
    .await;
    assert_eq!(reg["currency"], "EUR");
    assert_eq!(reg["location_name"], "Berlin store");
    let reg_id = reg["id"].as_str().unwrap().to_owned();
    let session = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/registers/{reg_id}/sessions"),
        a,
        json!({ "cash_minor": 10000 }),
        201,
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_owned();
    assert_eq!(session["status"], "open");
    let again = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/registers/{reg_id}/sessions"),
        a,
        json!({ "cash_minor": 0 }),
        409,
    )
    .await;
    assert_eq!(again["code"], "session_open");

    // The total first: nothing is kept.
    let q = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/quote"),
        a,
        json!({ "lines": [{ "sku": "MUG", "quantity": 2 }] }),
        200,
    )
    .await;
    let total = q["total_minor"].as_i64().unwrap();
    assert!(total >= 2400, "{q}");
    assert_eq!(q["lines"][0]["title"], "Mug");
    call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/quote"),
        a,
        json!({ "lines": [{ "sku": "NOPE", "quantity": 1 }] }),
        422,
    )
    .await;

    // Too little paid is refused, and nothing is sold.
    let short = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "sku": "MUG", "quantity": 2 }],
                "tenders": [{ "kind": "cash", "amount_minor": 100 }] }),
        422,
    )
    .await;
    assert_eq!(short["code"], "tender_short");
    // More than in the store is a conflict, though the warehouse has some.
    let eight = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "sku": "MUG", "quantity": 8 }],
                "tenders": [{ "kind": "cash", "amount_minor": 1_000_000 }] }),
        409,
    )
    .await;
    assert_eq!(eight["code"], "insufficient_stock", "{eight}");

    // Cash, with change.
    let sale = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "sku": "MUG", "quantity": 2 }],
                "tenders": [{ "kind": "cash", "amount_minor": total + 150 }] }),
        201,
    )
    .await;
    assert_eq!(sale["change_minor"], 150);
    assert_eq!(sale["order"]["financial_status"], "paid", "{sale}");
    assert_eq!(sale["order"]["fulfillment_status"], "fulfilled", "{sale}");
    let order_id = sale["order"]["id"].as_str().unwrap().to_owned();
    let inv = call(
        c,
        "acme",
        "GET",
        &format!("/api/inventory/{mug}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(on_hand(&inv, &store), 3, "{inv}");
    assert_eq!(on_hand(&inv, &warehouse), 10, "{inv}");
    assert_eq!(inv["reserved"], 0, "{inv}");

    // Card and cash, with a discount; card may not pay more than is due.
    call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "item_id": mug, "quantity": 1 }],
                "tenders": [{ "kind": "card", "amount_minor": 5000 }] }),
        422,
    )
    .await;
    let split = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "item_id": mug, "quantity": 1 }], "discount_minor": 200,
                "tenders": [{ "kind": "card", "amount_minor": 500 },
                            { "kind": "cash", "amount_minor": 1000 }] }),
        201,
    )
    .await;
    let split_total = split["total_minor"].as_i64().unwrap();
    assert_eq!(split["change_minor"], 1500 - split_total, "{split}");
    assert_eq!(split["order"]["financial_status"], "paid");

    // The receipt and the report.
    let r = call(
        c,
        "acme",
        "GET",
        &format!("/api/pos/sales/{order_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(r["register_name"], "Front desk");
    assert_eq!(r["cash_minor"], total + 150);
    let report = call(
        c,
        "acme",
        "GET",
        &format!("/api/pos/sessions/{sid}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(report["sales"], 2);
    assert_eq!(report["total_minor"], total + split_total);
    assert_eq!(report["card_minor"], 500);
    let cash_in = total + (split_total - 500);
    assert_eq!(report["cash_minor"], cash_in);
    assert_eq!(report["expected_cash_minor"], 10000 + cash_in);

    // The admin till: total, pay by card, the receipt.
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/pos")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Front desk") && html.contains("Go to the till"),
        "{html}"
    );
    let html = c
        .post(&format!("/admin/acme/pos/sessions/{sid}/total"))
        .header("cookie", &cookie)
        .form(&json!({ "currency": "EUR", "lines": "MUG", "discount": "" }))
        .send()
        .await
        .text();
    assert!(html.contains("Take payment"), "{html}");
    let res = c
        .post(&format!("/admin/acme/pos/sessions/{sid}/pay"))
        .header("cookie", &cookie)
        .form(&json!({ "currency": "EUR", "lines": "MUG", "discount": "",
                        "cash": "", "card": "", "method": "card" }))
        .send()
        .await;
    res.assert_status(303);
    let receipt = res.header("location").unwrap().to_owned();
    assert!(receipt.starts_with("/admin/acme/pos/sales/"), "{receipt}");
    let html = c
        .get(&receipt)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Receipt #") && html.contains("Card"),
        "{html}"
    );
    let html = c
        .post(&format!("/admin/acme/pos/sessions/{sid}/total"))
        .header("cookie", &cookie)
        .form(&json!({ "currency": "EUR", "lines": "MUG 0" }))
        .send()
        .await
        .text();
    assert!(html.contains("the quantity is 1 to 10000"), "{html}");

    // Closing with 5.00 missing; a closed session sells nothing.
    let report = call(
        c,
        "acme",
        "GET",
        &format!("/api/pos/sessions/{sid}"),
        a,
        Value::Null,
        200,
    )
    .await;
    let expected = report["expected_cash_minor"].as_i64().unwrap();
    assert_eq!(report["sales"], 3);
    let closed = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/close"),
        a,
        json!({ "cash_minor": expected - 500, "note": "short" }),
        200,
    )
    .await;
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["difference_minor"], -500);
    let refused = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/sessions/{sid}/sales"),
        a,
        json!({ "lines": [{ "sku": "MUG", "quantity": 1 }],
                "tenders": [{ "kind": "card", "amount_minor": 1 }] }),
        409,
    )
    .await;
    assert_eq!(refused["code"], "session_closed");
    let html = c
        .get(&format!("/admin/acme/pos/sessions/{sid}"))
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Missing") && !html.contains("Take payment"),
        "{html}"
    );

    // Events, and another shop sees none of it.
    for kind in [
        "pos_register.created",
        "pos_session.opened",
        "pos_session.closed",
    ] {
        let e = call(
            c,
            "acme",
            "GET",
            &format!("/api/events?kind={kind}"),
            a,
            Value::Null,
            200,
        )
        .await;
        assert_eq!(e["items"].as_array().unwrap().len(), 1, "{kind}");
    }
    let e = call(
        c,
        "acme",
        "GET",
        "/api/events?kind=pos_sale.completed",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(e["items"].as_array().unwrap().len(), 3);
    let none = call(c, "other", "GET", "/api/pos/registers", a, Value::Null, 200).await;
    assert!(none["items"].as_array().unwrap().is_empty());
    for (method, path) in [
        ("GET", format!("/api/pos/sessions/{sid}")),
        ("GET", format!("/api/pos/sales/{order_id}")),
        ("POST", format!("/api/pos/registers/{reg_id}/sessions")),
    ] {
        call(
            c,
            "other",
            method,
            &path,
            a,
            json!({ "cash_minor": 0 }),
            404,
        )
        .await;
    }
    call(
        c,
        "other",
        "PATCH",
        &format!("/api/pos/registers/{reg_id}"),
        a,
        json!({ "active": false }),
        404,
    )
    .await;
    // Out of use: no session opens.
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/pos/registers/{reg_id}"),
        a,
        json!({ "active": false }),
        200,
    )
    .await;
    let inactive = call(
        c,
        "acme",
        "POST",
        &format!("/api/pos/registers/{reg_id}/sessions"),
        a,
        json!({ "cash_minor": 0 }),
        409,
    )
    .await;
    assert_eq!(inactive["code"], "register_inactive");
}
