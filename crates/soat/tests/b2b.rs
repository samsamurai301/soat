//! I8: B2B. Companies share prices, payment terms and a credit limit;
//! buyers ask for quotes, staff offer, buyers accept; quick order by SKU.
#![cfg(feature = "b2b")]

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

async fn register(c: &TestClient, email: &str) -> String {
    let s = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/register",
        None,
        json!({ "email": email, "password": "a long password" }),
        201,
    )
    .await;
    s["token"].as_str().unwrap().to_owned()
}

async fn customer_id(c: &TestClient, admin: &str, q: &str) -> String {
    call(
        c,
        "acme",
        "GET",
        &format!("/api/customers?q={q}"),
        Some(admin),
        Value::Null,
        200,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A stocked product; its id.
async fn product(c: &TestClient, admin: &str, loc: &Value, sku: &str, price: i64) -> String {
    let a = Some(admin);
    let id = call(
        c,
        "acme",
        "POST",
        "/api/products",
        a,
        json!({ "sku": sku, "title": sku, "price_minor": price, "currency": "EUR",
                "status": "active" }),
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
        &format!("/api/inventory/{id}/adjustments"),
        a,
        json!({ "location_id": loc, "delta": 500, "reason": "received" }),
        200,
    )
    .await;
    id
}

/// The company's balance once events are handled, waiting for `open`.
async fn balance(c: &TestClient, admin: &str, company: &str, open: i64) -> Value {
    let res = c.resources().await;
    let mut b = Value::Null;
    for _ in 0..50 {
        soat_events::dispatch_resources(&res).await.unwrap();
        b = call(
            c,
            "acme",
            "GET",
            &format!("/api/companies/{company}/balance"),
            Some(admin),
            Value::Null,
            200,
        )
        .await;
        if b["open_minor"] == open {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    b
}

#[tokio::test]
async fn companies_quotes_and_terms() {
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
    let bolt = product(c, &admin, &loc, "BOLT", 100).await;
    let nut = product(c, &admin, &loc, "NUT", 50).await;
    // Trade prices for the wholesale group.
    let trade = call(
        c,
        "acme",
        "POST",
        "/api/price-lists",
        a,
        json!({ "name": "Trade", "currency": "EUR", "priority": 5, "customer_tag": "wholesale" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/price-lists/{trade}/prices"),
        a,
        json!({ "prices": [{ "item_id": bolt, "price_minor": 80 }] }),
        200,
    )
    .await;

    // A company with 30 days to pay and a 100 EUR limit.
    call(
        c,
        "acme",
        "POST",
        "/api/companies",
        a,
        json!({ "name": "" }),
        422,
    )
    .await;
    let acme_ltd = call(
        c,
        "acme",
        "POST",
        "/api/companies",
        a,
        json!({ "name": "Bauhaus GmbH", "vat_number": "de 123 456 789", "price_tag": "Wholesale",
                "payment_terms_days": 30, "currency": "EUR", "credit_limit_minor": 10000 }),
        201,
    )
    .await;
    let co = acme_ltd["id"].as_str().unwrap().to_owned();
    assert_eq!(acme_ltd["vat_number"], "DE123456789");
    assert_eq!(acme_ltd["price_tag"], "wholesale");

    // Ada buys for it (admin), Bob too (buyer); both get the trade prices.
    let ada = register(c, "ada@example.com").await;
    let bob = register(c, "bob@example.com").await;
    let ada_id = customer_id(c, &admin, "ada").await;
    let bob_id = customer_id(c, &admin, "bob").await;
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/companies/{co}/members/{ada_id}"),
        a,
        json!({ "role": "admin" }),
        200,
    )
    .await;
    let m = call(
        c,
        "acme",
        "PUT",
        &format!("/api/companies/{co}/members/{bob_id}"),
        a,
        json!({}),
        200,
    )
    .await;
    assert_eq!(m["items"].as_array().unwrap().len(), 2);
    let cu = call(
        c,
        "acme",
        "GET",
        &format!("/api/customers/{bob_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(cu["tags"], json!(["wholesale"]));
    // A customer is in one company at most.
    let other_co = call(
        c,
        "acme",
        "POST",
        "/api/companies",
        a,
        json!({ "name": "Other" }),
        201,
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/companies/{other_co}/members/{bob_id}"),
        a,
        json!({}),
        409,
    )
    .await;
    call(
        c,
        "acme",
        "PUT",
        &format!("/api/companies/{co}/members/{bob_id}"),
        a,
        json!({ "role": "owner" }),
        422,
    )
    .await;

    // Bob sees his company but not what it owes; Ada sees both.
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/company",
        Some(&bob),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine["company"]["name"], "Bauhaus GmbH");
    assert!(mine["balance"].is_null());
    let mine = call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/company",
        Some(&ada),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(mine["balance"]["open_minor"], 0);
    assert_eq!(mine["members"].as_array().unwrap().len(), 2);

    // Bob asks for a quote on 50 bolts and 100 nuts.
    call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/quotes",
        Some(&bob),
        json!({ "lines": [{ "item_id": "prod_nope", "quantity": 1 }] }),
        422,
    )
    .await;
    let q = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/quotes",
        Some(&bob),
        json!({ "lines": [{ "item_id": bolt, "quantity": 50 }, { "item_id": nut, "quantity": 100 }],
                "note": "Monthly need" }),
        201,
    )
    .await;
    let qid = q["id"].as_str().unwrap().to_owned();
    assert_eq!(q["status"], "requested");
    assert_eq!(q["company_id"], co.as_str());
    assert_eq!(q["lines"][0]["sku"], "BOLT");
    // Not his to accept yet; not Ada's at all.
    call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/quotes/{qid}/accept"),
        Some(&bob),
        json!({}),
        409,
    )
    .await;
    call(
        c,
        "acme",
        "GET",
        &format!("/api/storefront/account/quotes/{qid}"),
        Some(&ada),
        Value::Null,
        404,
    )
    .await;

    // Staff offer: bolts at the trade price, nuts at 0.40, without an
    // address the draft waits for one.
    call(c, "acme", "POST", &format!("/api/customers/{bob_id}/addresses"), a,
         json!({ "first_name": "Bob", "last_name": "B", "company": "Bauhaus GmbH",
                 "line1": "Werkstr. 5", "postal_code": "10115", "city": "Berlin", "country": "DE" }),
         201).await;
    let offered = call(
        c,
        "acme",
        "POST",
        &format!("/api/quotes/{qid}/offer"),
        a,
        json!({ "lines": [{ "item_id": bolt, "quantity": 50 },
                          { "item_id": nut, "quantity": 100, "unit_price_minor": 40 }],
                "reply": "Our best price", "expires_in_days": 14 }),
        200,
    )
    .await;
    assert_eq!(offered["status"], "offered");
    let offer = &offered["offer"];
    assert_eq!(offer["subtotal_minor"], 50 * 80 + 100 * 40);
    assert_eq!(offer["vat_number"], "DE123456789");

    // Over the credit limit (8000 + tax > 10000? no: 8000 net), so lower
    // the limit to see it refused, then raise it.
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/companies/{co}"),
        a,
        json!({ "credit_limit_minor": 5000 }),
        200,
    )
    .await;
    let r = call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/quotes/{qid}/accept"),
        Some(&bob),
        json!({}),
        409,
    )
    .await;
    assert_eq!(r["code"], "credit_limit_exceeded");
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/companies/{co}"),
        a,
        json!({ "status": "on_hold", "credit_limit_minor": 100000 }),
        200,
    )
    .await;
    let r = call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/quotes/{qid}/accept"),
        Some(&bob),
        json!({}),
        409,
    )
    .await;
    assert_eq!(r["code"], "company_on_hold");
    call(
        c,
        "acme",
        "PATCH",
        &format!("/api/companies/{co}"),
        a,
        json!({ "status": "active" }),
        200,
    )
    .await;

    // Accepted: the order is placed, unpaid, due in 30 days.
    let done = call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/quotes/{qid}/accept"),
        Some(&bob),
        json!({}),
        201,
    )
    .await;
    assert_eq!(done["quote"]["status"], "accepted");
    let order_id = done["order_id"].as_str().unwrap().to_owned();
    let order = call(
        c,
        "acme",
        "GET",
        &format!("/api/orders/{order_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(order["financial_status"], "pending");
    let total = order["total_minor"].as_i64().unwrap();
    call(
        c,
        "acme",
        "POST",
        &format!("/api/storefront/account/quotes/{qid}/accept"),
        Some(&bob),
        json!({}),
        409,
    )
    .await;

    // The ledger owes it, not yet overdue.
    let b = balance(c, &admin, &co, total).await;
    assert_eq!(b["open_minor"], total);
    assert_eq!(b["overdue_minor"], 0);
    assert_eq!(b["available_minor"], 100000 - total);
    assert_eq!(b["orders"][0]["state"], "open");
    // Paid: nothing is owed.
    // The transfer arrives; staff record it.
    let m = call(
        c,
        "acme",
        "POST",
        "/api/payment-methods",
        a,
        json!({ "provider": "manual", "name": "Invoice" }),
        201,
    )
    .await;
    let pay = call(
        c,
        "acme",
        "POST",
        "/api/payments",
        a,
        json!({ "method_id": m["id"], "reference": order_id, "amount_minor": total,
                           "currency": "EUR" }),
        201,
    )
    .await;
    call(
        c,
        "acme",
        "POST",
        &format!("/api/payments/{}/mark-paid", pay["id"].as_str().unwrap()),
        a,
        json!({}),
        200,
    )
    .await;
    let b = balance(c, &admin, &co, 0).await;
    assert_eq!(b["open_minor"], 0);
    assert_eq!(b["orders"][0]["state"], "paid");

    // Declining: staff refuse a second request.
    let q2 = call(
        c,
        "acme",
        "POST",
        "/api/storefront/account/quotes",
        Some(&ada),
        json!({ "lines": [{ "item_id": nut, "quantity": 5 }] }),
        201,
    )
    .await;
    let q2 = q2["id"].as_str().unwrap();
    let d = call(
        c,
        "acme",
        "POST",
        &format!("/api/quotes/{q2}/decline"),
        a,
        json!({ "reply": "Too few" }),
        200,
    )
    .await;
    assert_eq!(d["status"], "declined");
    call(
        c,
        "acme",
        "POST",
        &format!("/api/quotes/{q2}/offer"),
        a,
        json!({}),
        409,
    )
    .await;
    let waiting = call(
        c,
        "acme",
        "GET",
        "/api/quotes?status=requested",
        a,
        Value::Null,
        200,
    )
    .await;
    assert!(waiting["items"].as_array().unwrap().is_empty());

    // Leaving the company takes the price group away.
    call(
        c,
        "acme",
        "DELETE",
        &format!("/api/companies/{co}/members/{bob_id}"),
        a,
        Value::Null,
        204,
    )
    .await;
    let cu = call(
        c,
        "acme",
        "GET",
        &format!("/api/customers/{bob_id}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(cu["tags"], json!([]));
    call(
        c,
        "acme",
        "GET",
        "/api/storefront/account/company",
        Some(&bob),
        Value::Null,
        404,
    )
    .await;

    // Every step is in the event log.
    for kind in [
        "company.created",
        "company.updated",
        "company.member_added",
        "company.member_removed",
        "quote.requested",
        "quote.offered",
        "quote.accepted",
        "quote.declined",
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
        assert!(!e["items"].as_array().unwrap().is_empty(), "{kind}");
    }

    // Another shop sees none of it.
    for path in [
        format!("/api/companies/{co}"),
        format!("/api/companies/{co}/balance"),
        format!("/api/quotes/{qid}"),
    ] {
        call(c, "other", "GET", &path, a, Value::Null, 404).await;
    }
    call(
        c,
        "other",
        "PATCH",
        &format!("/api/companies/{co}"),
        a,
        json!({ "name": "x" }),
        404,
    )
    .await;
    let none = call(c, "other", "GET", "/api/companies", a, Value::Null, 200).await;
    assert!(none["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn quick_order_and_pages() {
    let (client, admin) = common::with_tenants(&["acme"]).await;
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
    product(c, &admin, &loc, "BOLT", 100).await;
    product(c, &admin, &loc, "NUT", 50).await;
    let ada = register(c, "ada@example.com").await;
    let me = format!("soat_customer={ada}");
    let post = |path: &'static str, form: Value| {
        let me = me.clone();
        async move {
            c.post(path)
                .header("x-soat-tenant", "acme")
                .header("cookie", &me)
                .form(&form)
                .send()
                .await
        }
    };
    let get = |path: String| {
        let me = me.clone();
        async move {
            c.get(&path)
                .header("x-soat-tenant", "acme")
                .header("cookie", &me)
                .send()
                .await
                .text()
        }
    };

    assert!(get("/account/quick-order".into())
        .await
        .contains("textarea"));
    // Unknown SKUs are named, and nothing goes in the cart.
    let res = post(
        "/account/quick-order",
        json!({ "lines": "BOLT 3\nSCREW 2", "action": "cart" }),
    )
    .await;
    res.assert_status(422);
    assert!(res.text().contains("SCREW"));
    let res = post(
        "/account/quick-order",
        json!({ "lines": "BOLT x", "action": "cart" }),
    )
    .await;
    res.assert_status(422);
    // Into the cart.
    let res = post(
        "/account/quick-order",
        json!({ "lines": "bolt 3\nNUT,10\nBOLT 2", "action": "cart" }),
    )
    .await;
    res.assert_status(303);
    let cart = res.cookie("soat_cart").unwrap().value().to_owned();
    let html = c
        .get("/cart")
        .header("x-soat-tenant", "acme")
        .header("cookie", &format!("{me}; soat_cart={cart}"))
        .send()
        .await
        .text();
    assert!(html.contains("BOLT") && html.contains("NUT"), "{html}");
    // Or as a quote.
    let res = post(
        "/account/quick-order",
        json!({ "lines": "NUT 400", "action": "quote", "note": "Big job" }),
    )
    .await;
    res.assert_status(303);
    let at = res.header("location").unwrap().to_owned();
    assert!(at.contains("/account/quotes/quo_"), "{at}");
    let html = get(at.clone()).await;
    assert!(
        html.contains("NUT") && html.contains("Asked") && html.contains("Big job"),
        "{html}"
    );
    let html = get("/account/quotes".into()).await;
    assert!(html.contains("Asked"), "{html}");

    // The admin offers from its quote page; the buyer accepts there.
    let qid = at.rsplit('/').next().unwrap().to_owned();
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let admin_cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get(&format!("/admin/acme/quotes/{qid}"))
        .header("cookie", &admin_cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Make an offer") && html.contains("ada@example.com"),
        "{html}"
    );
    let nut = call(
        c,
        "acme",
        "GET",
        &format!("/api/quotes/{qid}"),
        a,
        Value::Null,
        200,
    )
    .await["lines"][0]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ada_id = customer_id(c, &admin, "ada").await;
    call(
        c,
        "acme",
        "POST",
        &format!("/api/customers/{ada_id}/addresses"),
        a,
        json!({ "line1": "Werkstr. 5", "postal_code": "10115", "city": "Berlin", "country": "DE" }),
        201,
    )
    .await;
    let res = c
        .post(&format!("/admin/acme/quotes/{qid}/offer"))
        .header("cookie", &admin_cookie)
        .form(
            &json!({ "currency": "EUR", "item_0": nut, "quantity_0": "400", "price_0": "0.35",
                       "shipping": "9.90", "days": "7", "reply": "For you" }),
        )
        .send()
        .await;
    res.assert_status(303);
    let html = get(format!("/account/quotes/{qid}")).await;
    assert!(
        html.contains("Offer ready") && html.contains("For you") && html.contains("Accept"),
        "{html}"
    );
    let res = c
        .post(&format!("/account/quotes/{qid}/accept"))
        .header("x-soat-tenant", "acme")
        .header("cookie", &me)
        .send()
        .await;
    res.assert_status(303);
    assert!(res.header("location").unwrap().ends_with("/order"));
    let q = call(
        c,
        "acme",
        "GET",
        &format!("/api/quotes/{qid}"),
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(q["status"], "accepted");
    assert_eq!(q["offer"]["subtotal_minor"], 14000);
    assert_eq!(q["offer"]["shipping_price_minor"], 990);

    // The admin's companies page and a company's page.
    let res = c
        .post("/admin/acme/companies")
        .header("cookie", &admin_cookie)
        .form(
            &json!({ "name": "Bauhaus", "payment_terms_days": "14", "currency": "EUR",
                       "credit_limit": "500" }),
        )
        .send()
        .await;
    res.assert_status(303);
    let page = res.header("location").unwrap().to_owned();
    let res = c
        .post(&format!("{page}/add"))
        .header("cookie", &admin_cookie)
        .form(&json!({ "email": "ada@example.com", "role": "admin" }))
        .send()
        .await;
    res.assert_status(303);
    let html = c
        .get(&page)
        .header("cookie", &admin_cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("ada@example.com") && html.contains("500.00"),
        "{html}"
    );
    let html = c
        .get("/admin/acme/companies")
        .header("cookie", &admin_cookie)
        .send()
        .await
        .text();
    assert!(
        html.contains("Bauhaus") && html.contains("14 days"),
        "{html}"
    );
    // Ada's company page.
    let html = get("/account/company".into()).await;
    assert!(html.contains("Bauhaus") && html.contains("14"), "{html}");
}
