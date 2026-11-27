//! Recording deliveries for new events, and sending them.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::future::join_all;
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use soat_events::{backoff_secs, MAX_ATTEMPTS};
use soat_module::{Event, HandlerFuture, Subscriber};
use zapline::db::sqlx::{self, types::Json as SqlJson, PgPool};
use zapline::prelude::*;
use zapline::Resources;

use crate::target::{check_target, is_public, TargetPolicy};
use crate::{sign, DeliveryId};

/// How long a receiver gets to answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The `webhooks` subscriber: one delivery row per matching endpoint.
/// Idempotent, so a retried event adds nothing twice.
pub(crate) struct Recorder;

impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        "webhooks"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            let endpoints: Vec<(String,)> = sqlx::query_as(
                // Only endpoints that existed before the event: a new
                // endpoint never receives older events, nor its own
                // `webhook.created`.
                "SELECT id FROM webhook_endpoints \
                 WHERE ($1 = ANY(events) OR '*' = ANY(events)) \
                 AND created_at < (SELECT occurred_at FROM events WHERE id = $2)",
            )
            .bind(&event.kind)
            .bind(&event.id)
            .fetch_all(&mut *tx)
            .await?;
            for (endpoint,) in endpoints {
                sqlx::query(
                    "INSERT INTO webhook_deliveries (id, tenant_id, endpoint_id, event_id) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (endpoint_id, event_id) DO NOTHING",
                )
                .bind(DeliveryId::new().as_str())
                .bind(&event.tenant_id)
                .bind(endpoint)
                .bind(&event.id)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
    }
}

type HttpsClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

fn client() -> &'static HttpsClient {
    static CLIENT: OnceLock<HttpsClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder(TokioExecutor::new()).build(https)
    })
}

/// [`send_due`] with the pool and target policy from the app's resources.
pub async fn send_resources(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    let policy: TargetPolicy = res.get().unwrap_or(TargetPolicy::PublicHttps);
    send_due(&pool, policy).await
}

/// Webhook deliveries that are due, and those given up in the past day.
///
/// # Errors
///
/// When the database cannot be reached.
pub async fn backlog(pool: &PgPool) -> Result<soat_module::Backlog> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('soat.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let (waiting, oldest_secs, failed_last_day): (i64, f64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM webhook_deliveries \
             WHERE delivered_at IS NULL AND failed_at IS NULL AND next_attempt_at <= now()), \
           COALESCE((SELECT EXTRACT(EPOCH FROM now() - min(next_attempt_at)) FROM webhook_deliveries \
             WHERE delivered_at IS NULL AND failed_at IS NULL AND next_attempt_at <= now()), 0)::float8, \
           (SELECT count(*) FROM webhook_deliveries WHERE failed_at > now() - interval '1 day')",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(soat_module::Backlog {
        waiting,
        oldest_secs: oldest_secs.max(0.0),
        failed_last_day,
    })
}

type DueRow = (
    String,
    String,
    String,
    String,
    String,
    SqlJson<serde_json::Value>,
    i32,
);

/// Sends up to 50 due deliveries concurrently. Returns how many succeeded.
pub async fn send_due(pool: &PgPool, policy: TargetPolicy) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('soat.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let due: Vec<DueRow> = sqlx::query_as(
        "SELECT d.id, w.url, w.secret, e.id, e.kind, e.payload, d.attempts \
         FROM webhook_deliveries d \
         JOIN webhook_endpoints w ON w.id = d.endpoint_id \
         JOIN events e ON e.id = d.event_id \
         WHERE d.delivered_at IS NULL AND d.failed_at IS NULL AND d.next_attempt_at <= now() \
         ORDER BY d.id LIMIT 50 FOR UPDATE OF d SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;

    let results = join_all(due.into_iter().map(
        |(delivery, url, secret, event_id, kind, SqlJson(payload), attempts)| async move {
            let body = serde_json::json!({ "id": event_id, "type": kind, "data": payload });
            let outcome = post(&url, &secret, &delivery, &body, policy).await;
            (delivery, attempts, outcome)
        },
    ))
    .await;

    let mut delivered = 0;
    for (delivery, attempts, outcome) in results {
        let attempts = attempts + 1;
        match outcome {
            Ok(status) => {
                delivered += 1;
                sqlx::query(
                    "UPDATE webhook_deliveries SET delivered_at = now(), attempts = $2, \
                     last_status = $3, last_error = NULL WHERE id = $1",
                )
                .bind(&delivery)
                .bind(attempts)
                .bind(i32::from(status))
                .execute(&mut *tx)
                .await?;
            }
            Err((status, error)) => {
                sqlx::query(
                    "UPDATE webhook_deliveries SET attempts = $2, last_status = $3, \
                     last_error = $4, next_attempt_at = now() + make_interval(secs => $5), \
                     failed_at = CASE WHEN $2 >= $6 THEN now() END WHERE id = $1",
                )
                .bind(&delivery)
                .bind(attempts)
                .bind(status.map(i32::from))
                .bind(error)
                .bind(backoff_secs(attempts))
                .bind(MAX_ATTEMPTS)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(delivered)
}

/// POSTs one delivery. `Ok(status)` for a 2xx answer.
async fn post(
    url: &str,
    secret: &str,
    delivery: &str,
    body: &serde_json::Value,
    policy: TargetPolicy,
) -> std::result::Result<u16, (Option<u16>, String)> {
    let fail = |e: String| (None, e);
    let uri = check_target(url, policy).map_err(fail)?;
    if policy == TargetPolicy::PublicHttps {
        // Check what the name resolves to now, not when it was registered.
        let host = uri.host().unwrap_or_default().to_owned();
        let port = uri.port_u16().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| fail(format!("cannot resolve {host}: {e}")))?;
        for addr in addrs {
            if !is_public(addr.ip()) {
                return Err(fail(format!("{host} resolves to a private address")));
            }
        }
    }
    let bytes = serde_json::to_vec(body).map_err(|e| fail(e.to_string()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let request = http::Request::post(uri)
        .header("content-type", "application/json")
        .header("user-agent", "SOAT-Webhooks/1")
        .header("soat-delivery", delivery)
        .header("soat-signature", sign(secret, now, &bytes))
        .body(Full::new(Bytes::from(bytes)))
        .map_err(|e| fail(e.to_string()))?;
    let response = tokio::time::timeout(TIMEOUT, client().request(request))
        .await
        .map_err(|_| fail("timed out after 10 s".into()))?
        .map_err(|e| fail(e.to_string()))?;
    let status = response.status().as_u16();
    // Drain the body so the connection can be reused; its content is ignored.
    let _ = tokio::time::timeout(TIMEOUT, response.into_body().collect()).await;
    if (200..300).contains(&status) {
        Ok(status)
    } else {
        Err((Some(status), format!("the receiver answered {status}")))
    }
}
