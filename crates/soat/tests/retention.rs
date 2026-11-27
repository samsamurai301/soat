//! J4: data retention.
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
        "PUT" => client.put(path),
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

fn rule<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no rule {key} in {view}"))
}

async fn cart(c: &TestClient, shop: &str) -> String {
    call(
        c,
        shop,
        "POST",
        "/api/storefront/carts",
        None,
        json!({ "currency": "EUR" }),
        201,
    )
    .await["cart"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Direct SQL runs as the app's role, under row-level security: in `shop`.
async fn enter(conn: &mut PgConnection, shop: &str) {
    sqlx::query("SELECT set_config('soat.tenant', $1, false)")
        .bind(shop)
        .execute(conn)
        .await
        .unwrap();
}

async fn count(conn: &mut PgConnection, sql: &str, bind: &str) -> i64 {
    sqlx::query_as::<_, (i64,)>(sql)
        .bind(bind)
        .fetch_one(conn)
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn old_data_is_swept_by_the_shops_rules() {
    let (c, url) = common::client_and_url().await;
    let admin = common::setup_admin(&c).await;
    common::create_tenants(&c, &admin, &["acme", "other"]).await;
    let a = Some(admin.as_str());
    let mut conn = PgConnection::connect(&url).await.unwrap();

    let view = call(&c, "acme", "GET", "/api/retention", a, Value::Null, 200).await;
    let keys: Vec<&str> = view["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    for k in [
        "events",
        "expired_sessions",
        "abandoned_carts",
        "sent_emails",
        "consent_records",
        "closed_privacy_requests",
        "inactive_customers",
    ] {
        assert!(keys.contains(&k), "{k} missing from {keys:?}");
    }
    assert_eq!(rule(&view, "abandoned_carts")["days"], 90);
    assert_eq!(rule(&view, "abandoned_carts")["due"], 0);
    assert_eq!(rule(&view, "events")["days"], Value::Null);
    assert_eq!(rule(&view, "inactive_customers")["days"], Value::Null);
    assert_eq!(view["last_run_at"], Value::Null);

    // An old cart in each shop, a fresh one in acme.
    let old = cart(&c, "acme").await;
    let fresh = cart(&c, "acme").await;
    let theirs = cart(&c, "other").await;
    for (shop, id) in [("acme", &old), ("other", &theirs)] {
        enter(&mut conn, shop).await;
        sqlx::query("UPDATE carts SET updated_at = now() - interval '100 days' WHERE id = $1")
            .bind(id)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    // A customer gone quiet for years, and one who just joined.
    for email in ["quiet@example.com", "new@example.com"] {
        call(
            &c,
            "acme",
            "POST",
            "/api/storefront/account/register",
            None,
            json!({ "email": email, "password": "a long password" }),
            201,
        )
        .await;
    }
    enter(&mut conn, "acme").await;
    sqlx::query(
        "UPDATE customers SET created_at = now() - interval '800 days' \
         WHERE email = 'quiet@example.com'",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE customer_sessions SET created_at = now() - interval '800 days', \
         expires_at = now() - interval '770 days' WHERE customer_id = \
         (SELECT id FROM customers WHERE email = 'quiet@example.com')",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let view = call(&c, "acme", "GET", "/api/retention", a, Value::Null, 200).await;
    assert_eq!(rule(&view, "abandoned_carts")["due"], 1, "{view}");
    assert_eq!(rule(&view, "expired_sessions")["due"], 1);
    assert_eq!(
        rule(&view, "inactive_customers")["due"],
        0,
        "kept by default"
    );

    // Within each rule's limits only.
    for days in [
        json!({ "abandoned_carts": 3 }),
        json!({ "inactive_customers": 100 }),
        json!({ "events": 40000 }),
        json!({ "nope": 30 }),
    ] {
        call(
            &c,
            "acme",
            "PUT",
            "/api/retention",
            a,
            json!({ "days": days }),
            422,
        )
        .await;
    }
    let view = call(
        &c,
        "acme",
        "PUT",
        "/api/retention",
        a,
        json!({ "days": { "inactive_customers": 400, "events": 365, "closed_privacy_requests": 365, "consent_records": null } }),
        200,
    )
    .await;
    assert_eq!(rule(&view, "inactive_customers")["due"], 1);
    assert_eq!(rule(&view, "events")["days"], 365);
    assert_eq!(rule(&view, "consent_records")["days"], Value::Null);
    assert_eq!(rule(&view, "abandoned_carts")["days"], 90, "left as it was");

    // Only staff who may erase sweep by hand.
    let key = call(
        &c,
        "acme",
        "POST",
        "/api/api-keys",
        a,
        json!({ "name": "audit", "permissions": ["privacy:read"] }),
        201,
    )
    .await["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        &c,
        "acme",
        "GET",
        "/api/retention",
        Some(&key),
        Value::Null,
        200,
    )
    .await;
    call(
        &c,
        "acme",
        "POST",
        "/api/retention/sweep",
        Some(&key),
        Value::Null,
        403,
    )
    .await;
    call(
        &c,
        "acme",
        "PUT",
        "/api/retention",
        Some(&key),
        json!({ "days": {} }),
        403,
    )
    .await;

    let report = call(
        &c,
        "acme",
        "POST",
        "/api/retention/sweep",
        a,
        Value::Null,
        200,
    )
    .await;
    assert_eq!(
        report["removed"],
        json!({ "abandoned_carts": 1, "expired_sessions": 1, "inactive_customers": 1 }),
        "{report}"
    );
    assert_eq!(report["total"], 3);
    let carts = "SELECT count(*) FROM carts WHERE id = $1";
    assert_eq!(count(&mut conn, carts, &old).await, 0);
    assert_eq!(count(&mut conn, carts, &fresh).await, 1);
    enter(&mut conn, "other").await;
    assert_eq!(count(&mut conn, carts, &theirs).await, 1, "another shop's");
    enter(&mut conn, "acme").await;
    let customers = "SELECT count(*) FROM customers WHERE email = $1";
    assert_eq!(count(&mut conn, customers, "quiet@example.com").await, 0);
    assert_eq!(count(&mut conn, customers, "new@example.com").await, 1);
    let events = "SELECT count(*) FROM events WHERE tenant_id = 'acme' AND kind = $1";
    assert_eq!(count(&mut conn, events, "retention.swept").await, 1);
    assert_eq!(count(&mut conn, events, "customer.erased").await, 1);

    let view = call(&c, "acme", "GET", "/api/retention", a, Value::Null, 200).await;
    assert!(view["last_run_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(view["last_report"], report["removed"]);
    assert!(view["rules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["due"] == 0));

    // Back to the default.
    let view = call(
        &c,
        "acme",
        "PUT",
        "/api/retention",
        a,
        json!({ "days": {}, "reset": ["inactive_customers"] }),
        200,
    )
    .await;
    assert_eq!(rule(&view, "inactive_customers")["days"], Value::Null);

    // The daily task sweeps the other shop; acme was swept just now.
    let swept = soat_compliance::retention::sweep_due(&c.resources().await)
        .await
        .unwrap();
    assert_eq!(swept, 1);
    enter(&mut conn, "other").await;
    assert_eq!(count(&mut conn, carts, &theirs).await, 0);
    let other = call(&c, "other", "GET", "/api/retention", a, Value::Null, 200).await;
    assert_eq!(other["last_report"], json!({ "abandoned_carts": 1 }));
    assert_eq!(rule(&other, "events")["days"], Value::Null, "its own rules");
    assert_eq!(
        soat_compliance::retention::sweep_due(&c.resources().await)
            .await
            .unwrap(),
        0,
        "once a day"
    );
}

#[tokio::test]
async fn staff_set_retention_in_the_admin() {
    let (c, admin) = common::with_tenants(&["acme"]).await;
    let res = c
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let html = c
        .get("/admin/acme/privacy")
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("How long data is kept"), "{html}");
    assert!(
        html.contains("name=\"days_abandoned_carts\" value=\"90\""),
        "{html}"
    );
    assert!(html.contains("name=\"days_events\" value=\"\""), "{html}");

    let view = call(
        &c,
        "acme",
        "GET",
        "/api/retention",
        Some(&admin),
        Value::Null,
        200,
    )
    .await;
    let mut form = serde_json::Map::new();
    for r in view["rules"].as_array().unwrap() {
        let key = r["key"].as_str().unwrap();
        let days = match &r["days"] {
            Value::Null => String::new(),
            d => d.to_string(),
        };
        form.insert(format!("days_{key}"), json!(days));
    }
    form.insert("days_abandoned_carts".into(), json!("30"));
    let res = c
        .post("/admin/acme/privacy/retention")
        .header("cookie", &cookie)
        .form(&Value::Object(form.clone()))
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 303, "{}", res.text());
    let view = call(
        &c,
        "acme",
        "GET",
        "/api/retention",
        Some(&admin),
        Value::Null,
        200,
    )
    .await;
    assert_eq!(rule(&view, "abandoned_carts")["days"], 30);
    assert_eq!(rule(&view, "sent_emails")["days"], 730, "unchanged");
    assert_eq!(rule(&view, "events")["days"], Value::Null, "still kept");

    form.insert("days_abandoned_carts".into(), json!("2"));
    let res = c
        .post("/admin/acme/privacy/retention")
        .header("cookie", &cookie)
        .form(&Value::Object(form))
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 422);
    assert!(res.text().contains("abandoned_carts is kept 7 to"));

    let res = c
        .post("/admin/acme/privacy/retention/sweep")
        .header("cookie", &cookie)
        .send()
        .await;
    assert_eq!(res.status().as_u16(), 200);
    assert!(res.text().contains("Swept: 0 records removed."));
}

#[test]
fn every_rule_is_well_formed() {
    let modules = soat::modules(soat::Settings::for_tests("t"));
    modules.validate().unwrap();
    let map = modules.retention();
    assert!(map.all().len() >= 6);
    for (_, r) in map.all() {
        assert!(r.expired.contains("$2"), "{} never uses its days", r.key);
    }
}
