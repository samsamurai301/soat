//! Domain events: a transactional outbox with at-least-once delivery.
//!
//! 1. A module calls [`publish`] inside its tenant transaction, so the event
//!    exists exactly when the change commits.
//! 2. [`dispatch_once`] (run every second by the module, or by tests) fans
//!    each new event out into one delivery per interested subscriber, then
//!    runs due deliveries. A failed delivery is retried with exponential
//!    backoff (1 s, 2 s, 4 s ... capped at 1 h) and given up after
//!    [`MAX_ATTEMPTS`]. Rows are claimed with `FOR UPDATE SKIP LOCKED`, so
//!    several SOAT processes can dispatch side by side.
//!
//! Events carry their actor, so the log doubles as the audit log
//! (`GET /api/events`, served by `soat-tenancy`).

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use serde::Serialize;
use soat_core::id_kind;
use soat_module::{Backlog, Event, Module, Subscribers};
use zapline::db::sqlx::{self, types::Json as SqlJson, PgConnection, PgPool};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::tasks::Schedule;
use zapline::Resources;

id_kind!(EventId, EventKind, "evt");

/// Deliveries are given up after this many failed attempts.
pub const MAX_ATTEMPTS: i32 = 20;

/// Records `kind` (`resource.verb`) with `payload` for the shop that `conn`'s
/// transaction is scoped to. Call it in the same transaction as the change.
/// `actor` is who caused it (`user_...`, `key_...`), or `None` for the
/// system. Request handlers use `Tenant::publish`, which fills both in.
pub async fn publish(
    conn: &mut PgConnection,
    tenant_id: &str,
    actor: Option<&str>,
    kind: &str,
    payload: &impl Serialize,
) -> Result<EventId> {
    let id = EventId::new();
    let payload = serde_json::to_value(payload)
        .map_err(|e| Error::internal(format!("event payload: {e}")))?;
    sqlx::query(
        "INSERT INTO events (id, tenant_id, actor, kind, payload) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_str())
    .bind(tenant_id)
    .bind(actor)
    .bind(kind)
    .bind(SqlJson(payload))
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// The events module: outbox tables and the dispatcher. The shop's event
/// log API lives in `soat-tenancy`, which builds on this crate.
pub struct Events {
    poll: Option<Duration>,
}

impl Events {
    /// Dispatches every second.
    pub fn new() -> Self {
        Self {
            poll: Some(Duration::from_secs(1)),
        }
    }

    /// Never dispatches on its own: tests call [`dispatch_once`].
    pub fn manual() -> Self {
        Self { poll: None }
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Events {
    fn name(&self) -> &'static str {
        "events"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        // Its tables reference `tenants`; migration versions order that.
        &["auth"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn retention(&self) -> &'static [soat_module::Retention] {
        &[soat_module::Retention {
            key: "events",
            label: "The event log, which is also the audit log, once delivered",
            table: "events",
            expired: "occurred_at < now() - make_interval(days => $2) AND fanned_out AND NOT EXISTS (SELECT 1 FROM event_deliveries d \
                      WHERE d.event_id = events.id AND d.delivered_at IS NULL AND d.failed_at IS NULL)",
            default_days: None,
            min_days: 90,
        }]
    }

    fn mount(&self, app: App) -> App {
        match self.poll {
            Some(every) => app.schedule(
                Schedule::every(every).name("soat-events"),
                |res: Resources| async move { dispatch_resources(&res).await.map(|_| ()) },
            ),
            None => app,
        }
    }
}

/// [`dispatch_once`] with the pool and subscribers from the app's resources.
pub async fn dispatch_resources(res: &Resources) -> Result<usize> {
    let pool: PgPool = res.require()?;
    let subscribers: Subscribers = res.get().unwrap_or_default();
    dispatch_once(&pool, &subscribers, res).await
}

/// Fans out new events and runs due deliveries once. Returns how many
/// deliveries succeeded.
pub async fn dispatch_once(
    pool: &PgPool,
    subscribers: &Subscribers,
    resources: &Resources,
) -> Result<usize> {
    fan_out(pool, subscribers).await?;
    deliver(pool, subscribers, resources).await
}

/// The two queues behind events: new events not fanned out to their
/// subscribers yet, and subscriber deliveries that are due.
///
/// # Errors
///
/// When the database cannot be reached.
pub async fn backlog(pool: &PgPool) -> Result<(Backlog, Backlog)> {
    let mut tx = system_tx(pool).await?;
    let (waiting, oldest_secs): (i64, f64) = sqlx::query_as(
        "SELECT count(*), COALESCE(EXTRACT(EPOCH FROM now() - min(occurred_at)), 0)::float8 \
         FROM events WHERE NOT fanned_out",
    )
    .fetch_one(&mut *tx)
    .await?;
    let events = Backlog {
        waiting,
        oldest_secs: oldest_secs.max(0.0),
        failed_last_day: 0,
    };
    let (waiting, oldest_secs, failed_last_day): (i64, f64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM event_deliveries \
             WHERE delivered_at IS NULL AND failed_at IS NULL AND next_attempt_at <= now()), \
           COALESCE((SELECT EXTRACT(EPOCH FROM now() - min(next_attempt_at)) FROM event_deliveries \
             WHERE delivered_at IS NULL AND failed_at IS NULL AND next_attempt_at <= now()), 0)::float8, \
           (SELECT count(*) FROM event_deliveries WHERE failed_at > now() - interval '1 day')",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let deliveries = Backlog {
        waiting,
        oldest_secs: oldest_secs.max(0.0),
        failed_last_day,
    };
    Ok((events, deliveries))
}

async fn system_tx(pool: &PgPool) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('soat.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

async fn fan_out(pool: &PgPool, subscribers: &Subscribers) -> Result<()> {
    let mut tx = system_tx(pool).await?;
    let new: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, tenant_id, kind FROM events WHERE NOT fanned_out \
         ORDER BY id LIMIT 500 FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, tenant_id, kind) in &new {
        for sub in subscribers.all().iter().filter(|s| s.wants(kind)) {
            sqlx::query(
                "INSERT INTO event_deliveries (event_id, tenant_id, subscriber) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(tenant_id)
            .bind(sub.name())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE events SET fanned_out = TRUE WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

type DueRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    SqlJson<serde_json::Value>,
    i32,
);

async fn deliver(pool: &PgPool, subscribers: &Subscribers, resources: &Resources) -> Result<usize> {
    let mut tx = system_tx(pool).await?;
    let due: Vec<DueRow> = sqlx::query_as(
        "SELECT d.subscriber, e.id, e.tenant_id, e.actor, e.kind, e.payload, d.attempts \
         FROM event_deliveries d JOIN events e ON e.id = d.event_id \
         WHERE d.delivered_at IS NULL AND d.failed_at IS NULL AND d.next_attempt_at <= now() \
         ORDER BY d.event_id LIMIT 100 FOR UPDATE OF d SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut delivered = 0;
    for (subscriber, id, tenant_id, actor, kind, SqlJson(payload), attempts) in due {
        let event = Event {
            id,
            tenant_id,
            actor,
            kind,
            payload,
        };
        let outcome = match subscribers.get(&subscriber) {
            None => Err(format!("no subscriber named {subscriber} in this build")),
            Some(sub) => match AssertUnwindSafe(sub.handle(&event, resources))
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("the handler panicked".to_owned()),
            },
        };
        match outcome {
            Ok(()) => {
                delivered += 1;
                sqlx::query(
                    "UPDATE event_deliveries SET delivered_at = now(), attempts = attempts + 1, \
                     last_error = NULL WHERE event_id = $1 AND subscriber = $2",
                )
                .bind(&event.id)
                .bind(&subscriber)
                .execute(&mut *tx)
                .await?;
            }
            Err(error) => {
                let attempts = attempts + 1;
                sqlx::query(
                    "UPDATE event_deliveries SET attempts = $3, last_error = $4, \
                     next_attempt_at = now() + make_interval(secs => $5), \
                     failed_at = CASE WHEN $3 >= $6 THEN now() END \
                     WHERE event_id = $1 AND subscriber = $2",
                )
                .bind(&event.id)
                .bind(&subscriber)
                .bind(attempts)
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

/// Seconds before retry number `attempts`: 1, 2, 4 ... capped at an hour.
pub fn backoff_secs(attempts: i32) -> f64 {
    let exp = attempts.saturating_sub(1).clamp(0, 12);
    f64::from(1u32 << exp).min(3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_an_hour() {
        let delays: Vec<_> = [1, 2, 3, 4, 13, 20].map(backoff_secs).to_vec();
        assert_eq!(delays, [1.0, 2.0, 4.0, 8.0, 3600.0, 3600.0]);
    }
}
