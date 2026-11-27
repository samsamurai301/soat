//! K2: what operators watch. Request ids, `/metrics` behind its token with
//! the queues' backlog, and the queues in `/health`.

mod common;

use serde_json::Value;
use zapline::db::sqlx::{self, Connection, PgConnection};
use zapline::db::Database;
use zapline::test::TestClient;

const TOKEN: &str = "a scrape token for the tests";

/// A fresh app that Prometheus may scrape with [`TOKEN`], and its schema.
async fn scraped() -> (TestClient, String) {
    let url = common::fresh_database_url().await;
    let settings = soat::Settings {
        metrics_token: Some(TOKEN.to_owned()),
        ..soat::Settings::for_tests(common::SETUP_TOKEN)
    };
    (
        soat::app(Database::postgres(url.clone()), settings).test(),
        url,
    )
}

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let c = common::client().await;
    let res = c.get("/account/login").send().await;
    let id = res.header("x-request-id").expect("a request id");
    assert!(!id.is_empty());
    let res = c
        .get("/no-such-page")
        .header("x-request-id", "from-the-proxy-42")
        .send()
        .await;
    assert_eq!(res.header("x-request-id"), Some("from-the-proxy-42"));
}

#[tokio::test]
async fn metrics_are_hidden_without_a_token_setting() {
    let c = common::client().await;
    c.get("/metrics").send().await.assert_status(404);
    c.get("/metrics")
        .bearer("anything")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn metrics_need_the_token_and_show_the_queues() {
    let (c, url) = scraped().await;
    let admin = common::setup_admin(&c).await;
    common::create_tenants(&c, &admin, &["acme"]).await;
    c.get("/metrics").send().await.assert_status(401);
    c.get("/metrics")
        .bearer("wrong")
        .send()
        .await
        .assert_status(401);

    // A delivery given up an hour ago.
    let mut conn = PgConnection::connect(&url).await.unwrap();
    sqlx::query("SELECT set_config('soat.tenant', 'acme', false)")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO events (id, tenant_id, kind, payload, fanned_out) \
         VALUES ('evt_ops', 'acme', 'test.happened', '{}', TRUE)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO event_deliveries (event_id, tenant_id, subscriber, attempts, failed_at) \
         VALUES ('evt_ops', 'acme', 'nobody', 20, now() - interval '1 hour')",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let res = c.get("/metrics").bearer(TOKEN).send().await;
    res.assert_status(200);
    assert!(res
        .header("content-type")
        .is_some_and(|t| t.starts_with("text/plain; version=0.0.4")));
    let body = res.text();
    assert!(body.contains("http_requests_total{"), "{body}");
    assert!(body.contains("route=\"/api/tenants\""), "{body}");
    assert!(body.contains("scheduled_task_"), "{body}");
    for queue in ["events", "subscribers", "webhooks"] {
        assert!(
            body.contains(&format!("soat_queue_waiting{{queue=\"{queue}\"}}")),
            "{body}"
        );
    }
    assert!(
        body.contains("soat_queue_failed_last_day{queue=\"subscribers\"} 1"),
        "{body}"
    );
    // Scrapes and probes stay out of the HTTP metrics.
    c.get("/health").send().await;
    let body = c.get("/metrics").bearer(TOKEN).send().await.text();
    assert!(!body.contains("route=\"/metrics\""), "{body}");
    assert!(!body.contains("route=\"/health\""), "{body}");
}

#[tokio::test]
async fn health_reports_the_queues() {
    let c = common::client().await;
    let report: Value = c.get("/health").send().await.assert_status(200).json();
    assert_eq!(report["status"], "up", "{report}");
    for check in ["database", "events", "webhooks"] {
        assert_eq!(report["checks"][check]["status"], "up", "{report}");
    }
}
