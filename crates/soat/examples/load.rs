//! K5: the load baseline. Shoppers browse, fill carts and check out while
//! staff list orders, all at once against one SOAT on a fresh Postgres
//! schema; each step's p95 must stay within its budget.
//!
//! ```sh
//! cargo run --release -p soat --example load              # 20 shoppers, 20 s
//! SHOPPERS=50 SECONDS=60 cargo run --release -p soat --example load
//! ```
//!
//! Requests go through the whole app (guards, sessions, templates, SQL with
//! row-level security, background events), in process, so the numbers are
//! SOAT's and Postgres's, not the network's. It exits non-zero when a step
//! misses its budget or answers an unexpected status. See
//! `docs/performance.md`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zapline::db::Database;
use zapline::test::{TestClient, TestResponse};

/// Each step's p95 budget, in milliseconds, for a release build.
const BUDGETS: [(&str, u64); 10] = [
    ("home", 50),
    ("search", 60),
    ("collection", 60),
    ("product", 50),
    ("cart add", 60),
    ("cart", 50),
    ("checkout", 80),
    ("checkout address", 120),
    ("place order", 250),
    ("api orders", 80),
];

const SETUP: &str = "load-setup-token";
const PRODUCTS: usize = 40;

#[derive(Default)]
struct Samples {
    times: BTreeMap<&'static str, Vec<Duration>>,
    errors: BTreeMap<&'static str, Vec<String>>,
}

type Shared = Arc<Mutex<Samples>>;

/// Runs `send`, records its time under `step`, and checks the status.
async fn timed<F, Fut>(samples: &Shared, step: &'static str, want: u16, send: F) -> TestResponse
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = TestResponse>,
{
    let start = Instant::now();
    let res = send().await;
    let took = start.elapsed();
    let mut s = samples.lock().unwrap();
    s.times.entry(step).or_default().push(took);
    if res.status().as_u16() != want {
        let errors = s.errors.entry(step).or_default();
        if errors.len() < 3 {
            errors.push(format!(
                "{}: {}",
                res.status(),
                res.text().chars().take(200).collect::<String>()
            ));
        } else {
            errors.push(String::new());
        }
    }
    res
}

async fn api(c: &TestClient, token: &str, method: &str, path: &str, body: Value) -> Value {
    let req = match method {
        "POST" => c.post(path),
        "PUT" => c.put(path),
        _ => c.get(path),
    }
    .header("x-soat-tenant", "acme")
    .bearer(token);
    let req = if body.is_null() { req } else { req.json(&body) };
    let res = req.send().await;
    assert!(res.status().is_success(), "{method} {path}: {}", res.text());
    res.json()
}

/// A shop with products in stock, a collection, shipping to Germany and
/// payment by invoice. Returns the staff token and the products' handles.
async fn shop(c: &TestClient) -> (String, Vec<(String, String)>) {
    let session: Value = c
        .post("/api/setup")
        .json(&json!({ "setup_token": SETUP, "email": "staff@example.com", "password": "a long load password" }))
        .send()
        .await
        .json();
    let t = session["token"].as_str().unwrap().to_owned();
    c.post("/api/tenants")
        .bearer(&t)
        .json(&json!({ "id": "acme", "name": "Acme Coffee" }))
        .send()
        .await
        .assert_status(201);
    let loc = api(
        c,
        &t,
        "POST",
        "/api/locations",
        json!({ "name": "W", "country": "DE" }),
    )
    .await;
    let mut products = Vec::new();
    for i in 0..PRODUCTS {
        let p = api(
            c,
            &t,
            "POST",
            "/api/products",
            json!({
                "sku": format!("BEAN-{i}"), "title": format!("Coffee beans no. {i}"),
                "price_minor": 1290 + i64::try_from(i).unwrap() * 10, "currency": "EUR",
                "description": "Roasted *weekly*. Notes of cocoa and cherry."
            }),
        )
        .await;
        let id = p["id"].as_str().unwrap().to_owned();
        api(
            c,
            &t,
            "POST",
            &format!("/api/inventory/{id}/adjustments"),
            json!({ "location_id": loc["id"], "delta": 1_000_000, "reason": "received" }),
        )
        .await;
        products.push((id, p["handle"].as_str().unwrap().to_owned()));
    }
    api(
        c,
        &t,
        "POST",
        "/api/collections",
        json!({ "title": "Coffee" }),
    )
    .await;
    let zone = api(
        c,
        &t,
        "POST",
        "/api/shipping/zones",
        json!({ "name": "DE", "countries": ["DE"] }),
    )
    .await;
    api(
        c,
        &t,
        "POST",
        &format!("/api/shipping/zones/{}/rates", zone["id"].as_str().unwrap()),
        json!({ "name": "Standard", "currency": "EUR", "price_minor": 495, "min_days": 1, "max_days": 3 }),
    )
    .await;
    api(
        c,
        &t,
        "POST",
        "/api/payment-methods",
        json!({ "provider": "manual", "name": "Invoice", "settings": { "instructions": "Pay in 14 days" } }),
    )
    .await;
    (t, products)
}

/// One shopper's visit, start to order.
async fn visit(c: &TestClient, s: &Shared, n: usize, products: &[(String, String)]) {
    let shop = |r: zapline::test::TestRequest| r.header("x-soat-tenant", "acme");
    let (id, handle) = &products[n % products.len()];
    timed(s, "home", 200, || shop(c.get("/")).send()).await;
    timed(s, "search", 200, || shop(c.get("/search?q=coffee")).send()).await;
    timed(s, "collection", 200, || {
        shop(c.get("/collections/coffee")).send()
    })
    .await;
    timed(s, "product", 200, || {
        shop(c.get(&format!("/products/{handle}"))).send()
    })
    .await;
    let res = timed(s, "cart add", 303, || {
        shop(c.post("/cart/add"))
            .form(&json!({ "product_id": id, "quantity": "1" }))
            .send()
    })
    .await;
    let Some(cart) = res
        .cookie("soat_cart")
        .map(|k| format!("soat_cart={}", k.value()))
    else {
        return;
    };
    timed(s, "cart", 200, || {
        shop(c.get("/cart")).header("cookie", &cart).send()
    })
    .await;
    timed(s, "checkout", 200, || {
        shop(c.get("/checkout")).header("cookie", &cart).send()
    })
    .await;
    timed(s, "checkout address", 303, || {
        shop(c.post("/checkout"))
            .header("cookie", &cart)
            .form(&json!({
                "email": format!("shopper{n}@example.com"), "first_name": "Ada", "last_name": "L",
                "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE",
                "discount_code": ""
            }))
            .send()
    })
    .await;
    // The method on the page, as a buyer would pick it.
    let page = shop(c.get("/checkout"))
        .header("cookie", &cart)
        .send()
        .await
        .text();
    let Some(method) = page
        .split("name=\"payment_method_id\" value=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
    else {
        s.lock()
            .unwrap()
            .errors
            .entry("checkout")
            .or_default()
            .push("no payment method".into());
        return;
    };
    timed(s, "place order", 303, || {
        shop(c.post("/checkout/complete"))
            .header("cookie", &cart)
            .form(&json!({ "payment_method_id": method }))
            .send()
    })
    .await;
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let i = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i]
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    let shoppers = usize::try_from(env("SHOPPERS", 20)).unwrap_or(20);
    let seconds = env("SECONDS", 20);
    let url = soat_testing::fresh_database_url().await;
    let c = soat::app(Database::postgres(url), soat::Settings::for_tests(SETUP)).test();
    let (staff, products) = shop(&c).await;
    let products = Arc::new(products);
    let samples: Shared = Arc::default();
    println!("{shoppers} shoppers and 1 staff member for {seconds} s, {PRODUCTS} products");

    let until = Instant::now() + Duration::from_secs(seconds);
    let mut tasks = Vec::new();
    for shopper in 0..shoppers {
        let (c, s, products) = (c.clone(), samples.clone(), products.clone());
        tasks.push(tokio::spawn(async move {
            let mut n = shopper;
            while Instant::now() < until {
                visit(&c, &s, n, &products).await;
                n += shoppers;
            }
        }));
    }
    {
        let (c, s) = (c.clone(), samples.clone());
        tasks.push(tokio::spawn(async move {
            while Instant::now() < until {
                timed(&s, "api orders", 200, || {
                    c.get("/api/orders?limit=50")
                        .header("x-soat-tenant", "acme")
                        .bearer(&staff)
                        .send()
                })
                .await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    let s = samples.lock().unwrap();
    let mut failed = false;
    println!(
        "\n{:<18}{:>8}{:>10}{:>10}{:>10}{:>10}  result",
        "step", "count", "p50 ms", "p95 ms", "p99 ms", "budget"
    );
    for (step, budget) in BUDGETS {
        let mut times = s.times.get(step).cloned().unwrap_or_default();
        times.sort();
        let p95 = percentile(&times, 0.95);
        let errors = s.errors.get(step).map_or(0, Vec::len);
        let over = p95 > Duration::from_millis(budget);
        let result = if times.is_empty() {
            "no samples"
        } else if errors > 0 {
            "errors"
        } else if over {
            "over budget"
        } else {
            "ok"
        };
        failed |= result != "ok";
        println!(
            "{step:<18}{:>8}{:>10}{:>10}{:>10}{:>10}  {result}",
            times.len(),
            ms(percentile(&times, 0.5)),
            ms(p95),
            ms(percentile(&times, 0.99)),
            budget,
        );
    }
    for (step, errors) in &s.errors {
        println!("\n{step}: {} unexpected answers", errors.len());
        for e in errors.iter().filter(|e| !e.is_empty()) {
            println!("  {e}");
        }
    }
    let orders: usize = s.times.get("place order").map_or(0, Vec::len);
    #[allow(clippy::cast_precision_loss)]
    let rate = orders as f64 / seconds as f64;
    println!("\n{orders} orders placed, {rate:.1} a second");
    if failed {
        std::process::exit(1);
    }
}
