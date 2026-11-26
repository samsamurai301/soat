//! The shop's event log, which is also its audit log: every change, what
//! it was and who made it.

use soat_core::Permission;
use zapline::db::sqlx::{self, types::Json as SqlJson};
use zapline::db::{Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{Tenant, TenantListing};

/// See the shop's event and audit log.
pub const EVENTS_READ: Permission =
    Permission::new("events:read", "See the shop's event and audit log");

/// An event in the shop's log.
#[model]
pub struct EventView {
    pub id: String,
    pub kind: String,
    /// `user_...`, `key_...`, or null for the system.
    pub actor: Option<String>,
    /// When it happened, RFC 3339 in UTC.
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for EventView {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        let SqlJson(payload) = row.try_get("payload")?;
        Ok(Self {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            actor: row.try_get("actor")?,
            occurred_at: row.try_get("occurred_at")?,
            payload,
        })
    }
}

pub(crate) fn mount(app: App) -> App {
    app.mount(list_events)
}

/// The shop's events, newest first (`events:read`). `?kind=product.created`
/// and `?actor=user_...` filter.
#[get("/api/events", tag = "events")]
async fn list_events(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<EventView>>> {
    let mut tx = tenant.begin_for(&db, &EVENTS_READ).await?;
    let page = TenantListing::new(
        "events",
        "id, kind, actor, payload, \
         to_char(occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
         AS occurred_at",
    )
    .filter_text("kind")
    .filter_text("actor")
    .default_sort("-id")
    .page_size(50, 200)
    .fetch(&mut tx, &params)
    .await?;
    tx.commit().await?;
    Ok(Json(page))
}
