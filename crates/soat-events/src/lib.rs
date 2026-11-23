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
//! 3. `GET /api/events` lists a shop's events (`events:read`).

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use serde::Serialize;
use soat_core::{id_kind, Permission};
use soat_module::{Event, Module, Subscribers};
use soat_tenancy::{Tenant, TenantListing};
use zapline::db::sqlx::{self, types::Json as SqlJson, PgConnection, PgPool};
use zapline::db::{Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::tasks::Schedule;
use zapline::Resources;

id_kind!(EventId, EventKind, "evt");

/// Deliveries are given up after this many failed attempts.
pub const MAX_ATTEMPTS: i32 = 20;

/// See the shop's event log.
pub const EVENTS_READ: Permission = Permission::new("events:read", "See the shop's event log");

/// Records `kind` (`resource.verb`) with `payload` for the shop that `conn`'s
/// transaction is scoped to. Call it in the same transaction as the change.
pub async fn publish(
    conn: &mut PgConnection,
    tenant_id: &str,
    kind: &str,
    payload: &impl Serialize,
) -> Result<EventId> {
    let id = EventId::new();
    let payload = serde_json::to_value(payload)
        .map_err(|e| Error::internal(format!("event payload: {e}")))?;
    sqlx::query("INSERT INTO events (id, tenant_id, kind, payload) VALUES ($1, $2, $3, $4)")
        .bind(id.as_str())
        .bind(tenant_id)
        .bind(kind)
        .bind(SqlJson(payload))
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

/// The events module: outbox tables, the dispatcher and the events API.
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
        &["tenancy"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[EVENTS_READ]
    }

    fn mount(&self, app: App) -> App {
        let app = app.mount(list_events);
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
    String,
    SqlJson<serde_json::Value>,
    i32,
);

async fn deliver(pool: &PgPool, subscribers: &Subscribers, resources: &Resources) -> Result<usize> {
    let mut tx = system_tx(pool).await?;
    let due: Vec<DueRow> = sqlx::query_as(
        "SELECT d.subscriber, e.id, e.tenant_id, e.kind, e.payload, d.attempts \
         FROM event_deliveries d JOIN events e ON e.id = d.event_id \
         WHERE d.delivered_at IS NULL AND d.failed_at IS NULL AND d.next_attempt_at <= now() \
         ORDER BY d.event_id LIMIT 100 FOR UPDATE OF d SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut delivered = 0;
    for (subscriber, id, tenant_id, kind, SqlJson(payload), attempts) in due {
        let event = Event {
            id,
            tenant_id,
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

/// An event in the shop's log.
#[model]
pub struct EventView {
    pub id: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for EventView {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let SqlJson(payload) = row.try_get("payload")?;
        Ok(Self {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            payload,
        })
    }
}

/// The shop's events, newest first (`events:read`). `?kind=` filters.
#[get("/api/events", tag = "events")]
async fn list_events(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<EventView>>> {
    let mut tx = tenant.begin_for(&db, &EVENTS_READ).await?;
    let page = TenantListing::new("events", "id, kind, payload")
        .filter_text("kind")
        .default_sort("-id")
        .page_size(50, 200)
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
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
