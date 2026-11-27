//! What operators watch: request ids and structured request logs,
//! Prometheus metrics behind a token, and the background queues in health.

use std::sync::Arc;

use soat_module::Backlog;
use zapline::auth::constant_time_eq;
use zapline::db::sqlx::PgPool;
use zapline::db::Db;
use zapline::health::Health;
use zapline::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use zapline::http::HeaderValue;
use zapline::metrics::{Encoder, MetricKind, Metrics, MetricsHandle, CONTENT_TYPE_TEXT};
use zapline::middleware::SetRequestId;
use zapline::prelude::*;
use zapline::telemetry::Trace;

/// A queue whose oldest due item waited longer than this, in seconds, is
/// reported down in `/health` (the shop stays ready: it is not critical).
pub const STUCK_AFTER_SECS: f64 = 600.0;

/// Paths left out of request logs and HTTP metrics: probes and scrapes.
const QUIET: [&str; 3] = ["/health", "/health/*", "/metrics"];

/// The scrape token: `SOAT_METRICS_TOKEN`, or none and `/metrics` is 404.
#[derive(Clone)]
struct MetricsToken(Option<Arc<str>>);

/// Outermost: every request gets an id (an incoming `x-request-id` is
/// kept), which the request log and the response carry.
pub(crate) fn first(app: App) -> App {
    app.wrap(SetRequestId::new())
        .plugin(Trace::new().skip_paths(QUIET))
}

/// After the modules: HTTP metrics see every route, `/metrics` serves them
/// with the queues' backlog, and health reports the queues.
pub(crate) fn last(app: App, token: Option<String>) -> App {
    app.state(MetricsToken(token.filter(|t| !t.is_empty()).map(Arc::from)))
        .plugin(Metrics::new().endpoint(false).exclude_paths(QUIET))
        .get("/metrics", metrics)
        .plugin(
            Health::new()
                .check("database", zapline::db::DbCheck::new())
                .check("events", |res: Resources| async move {
                    let pool: PgPool = res.require()?;
                    let (events, deliveries) = soat_events::backlog(&pool).await?;
                    unstuck("events", events)?;
                    unstuck("subscribers", deliveries)
                })
                .critical(false)
                .check("webhooks", |res: Resources| async move {
                    let pool: PgPool = res.require()?;
                    unstuck("webhooks", soat_webhooks::backlog(&pool).await?)
                })
                .critical(false),
        )
}

fn unstuck(queue: &str, backlog: Backlog) -> Result<()> {
    if backlog.stuck(STUCK_AFTER_SECS) {
        return Err(Error::service_unavailable(format!(
            "{queue}: {} waiting, the oldest for {:.0} s",
            backlog.waiting, backlog.oldest_secs
        )));
    }
    Ok(())
}

/// Prometheus metrics: HTTP requests by route, the process, scheduled
/// tasks, and the queues behind events and webhooks (`soat_queue_*`).
/// Needs `Authorization: Bearer` with `SOAT_METRICS_TOKEN`; without that
/// setting it does not exist.
async fn metrics(
    State(token): State<MetricsToken>,
    State(handle): State<MetricsHandle>,
    db: Db,
    headers: HeaderMap,
) -> Result<Response> {
    let Some(expected) = token.0 else {
        return Err(Error::not_found());
    };
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !constant_time_eq(given, &*expected) {
        return Err(Error::unauthorized());
    }
    let mut body = handle.render();
    let (events, deliveries) = soat_events::backlog(&db).await?;
    let webhooks = soat_webhooks::backlog(&db).await?;
    queues(
        &mut body,
        &[
            ("events", events),
            ("subscribers", deliveries),
            ("webhooks", webhooks),
        ],
    );
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_TEXT));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// The `soat_queue_*` families.
fn queues(body: &mut String, all: &[(&str, Backlog)]) {
    let mut out = Encoder::new(body);
    out.header(
        "soat_queue_waiting",
        "Items due now and not done yet.",
        MetricKind::Gauge,
    );
    for (queue, b) in all {
        #[allow(clippy::cast_precision_loss)]
        out.sample("soat_queue_waiting", &[("queue", queue)], b.waiting as f64);
    }
    out.header(
        "soat_queue_oldest_seconds",
        "How long the oldest waiting item has been due.",
        MetricKind::Gauge,
    );
    for (queue, b) in all {
        out.sample(
            "soat_queue_oldest_seconds",
            &[("queue", queue)],
            b.oldest_secs,
        );
    }
    out.header(
        "soat_queue_failed_last_day",
        "Items given up after their last retry in the past 24 hours.",
        MetricKind::Gauge,
    );
    for (queue, b) in all {
        #[allow(clippy::cast_precision_loss)]
        out.sample(
            "soat_queue_failed_last_day",
            &[("queue", queue)],
            b.failed_last_day as f64,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_render_as_gauges() {
        let mut body = String::new();
        queues(
            &mut body,
            &[(
                "webhooks",
                Backlog {
                    waiting: 3,
                    oldest_secs: 12.5,
                    failed_last_day: 1,
                },
            )],
        );
        assert!(body.contains("# TYPE soat_queue_waiting gauge"));
        assert!(body.contains("soat_queue_waiting{queue=\"webhooks\"} 3"));
        assert!(body.contains("soat_queue_oldest_seconds{queue=\"webhooks\"} 12.5"));
        assert!(body.contains("soat_queue_failed_last_day{queue=\"webhooks\"} 1"));
    }

    #[test]
    fn a_queue_is_stuck_only_when_something_waits_too_long() {
        let b = |waiting, oldest_secs| Backlog {
            waiting,
            oldest_secs,
            failed_last_day: 0,
        };
        assert!(unstuck("q", b(0, 0.0)).is_ok());
        assert!(unstuck("q", b(5, 30.0)).is_ok());
        assert!(unstuck("q", b(1, STUCK_AFTER_SECS + 1.0)).is_err());
    }
}
