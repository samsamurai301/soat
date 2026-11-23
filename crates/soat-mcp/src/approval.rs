//! Approval mode: an assistant's changes wait for a person.

use serde_json::{json, Map, Value};
use soat_tenancy::{Scope, Tenant, TenantListing};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{answer, ProposalId, Shared, Tool, ASSISTANT_APPROVE, ASSISTANT_MANAGE, ASSISTANT_USE};

pub(crate) fn mount(app: App) -> App {
    app.mount((
        get_settings,
        put_settings,
        list_proposals,
        get_proposal,
        approve,
        reject,
    ))
}

/// Whether an assistant's changes wait for approval.
#[model]
#[derive(sqlx::FromRow)]
pub struct AssistantSettings {
    /// `writes`: every change waits for a person (the default); `off`:
    /// changes run at once, within the caller's permissions.
    pub approval: String,
}

/// A change an assistant asked for.
#[model]
pub struct Proposal {
    /// e.g. `prop_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The tool called, e.g. `update_product`.
    pub tool: String,
    pub method: String,
    /// The API path with its query.
    pub path: String,
    /// The request body, if any.
    pub body: Option<Value>,
    /// Why the assistant wants it, in its words.
    pub reason: String,
    /// `pending`, `approved` (running), `applied`, `failed` or `rejected`.
    pub status: String,
    /// The API key or user whose assistant asked.
    pub proposed_by: Option<String>,
    /// Who approved or rejected it.
    pub decided_by: Option<String>,
    /// The HTTP status the change answered once approved.
    pub result_status: Option<i32>,
    /// What the change answered once approved.
    pub result: Option<Value>,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub decided_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    tool: String,
    method: String,
    path: String,
    body: Option<SqlJson<Value>>,
    reason: String,
    status: String,
    proposed_by: Option<String>,
    decided_by: Option<String>,
    result_status: Option<i32>,
    result: Option<SqlJson<Value>>,
    created_at: String,
    decided_at: Option<String>,
}

impl From<Row> for Proposal {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            tool: r.tool,
            method: r.method,
            path: r.path,
            body: r.body.map(|b| b.0),
            reason: r.reason,
            status: r.status,
            proposed_by: r.proposed_by,
            decided_by: r.decided_by,
            result_status: r.result_status,
            result: r.result.map(|b| b.0),
            created_at: r.created_at,
            decided_at: r.decided_at,
        }
    }
}

impl Proposal {
    /// What events say about it: no bodies, which may hold secrets.
    fn event(&self) -> Value {
        json!({
            "id": self.id, "tool": self.tool, "method": self.method, "path": self.path,
            "status": self.status, "proposed_by": self.proposed_by,
            "decided_by": self.decided_by, "result_status": self.result_status,
        })
    }
}

const COLUMNS: &str = "id, tool, method, path, body, reason, status, proposed_by, decided_by, \
     result_status, result, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(decided_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS decided_at";

async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<AssistantSettings> {
    let s = sqlx::query_as::<_, AssistantSettings>(
        "SELECT approval FROM assistant_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(s.unwrap_or_else(|| AssistantSettings {
        approval: "writes".to_owned(),
    }))
}

/// Whether the shop wants changes approved.
pub(crate) async fn required(db: &Db, tenant: &Tenant) -> Result<bool> {
    let mut tx = tenant.begin(db).await?;
    let s = settings(&mut tx, tenant).await?;
    tx.commit().await?;
    Ok(s.approval != "off")
}

async fn proposal(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Proposal> {
    let id = ProposalId::parse(id).map_err(|_| Error::not_found())?;
    let row = sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM assistant_proposals WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)?;
    Ok(row.into())
}

/// Records a change for approval. Publishes `assistant_proposal.created`.
pub(crate) async fn propose(
    db: &Db,
    tenant: &Tenant,
    tool: &Tool,
    method: &Method,
    path: &str,
    body: Option<&Value>,
    reason: &str,
) -> Result<Proposal> {
    let mut tx = tenant.begin_for(db, &ASSISTANT_USE).await?;
    let id = ProposalId::new();
    let reason: String = reason.chars().take(2000).collect();
    sqlx::query(
        "INSERT INTO assistant_proposals (id, tenant_id, tool, method, path, body, reason, \
         proposed_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(&tool.name)
    .bind(method.as_str())
    .bind(path)
    .bind(body.map(SqlJson))
    .bind(reason.trim())
    .bind(tenant.actor_id())
    .execute(&mut *tx)
    .await?;
    let p = proposal(&mut tx, tenant, id.as_str()).await?;
    tenant
        .publish(&mut tx, "assistant_proposal.created", &p.event())
        .await?;
    tx.commit().await?;
    Ok(p)
}

/// The built-in tool that reports on a proposal.
pub(crate) const STATUS_TOOL: &str = "proposal_status";

pub(crate) fn status_tool() -> Value {
    json!({
        "name": STATUS_TOOL,
        "title": "proposal status",
        "description": "What became of a change that waited for approval: pending, applied \
                        (with the API's answer), failed or rejected.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The proposal id, prop_..." } },
            "required": ["id"],
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

pub(crate) async fn status(db: &Db, tenant: &Tenant, args: &Map<String, Value>) -> Result<Value> {
    let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut tx = tenant.begin_for(db, &ASSISTANT_USE).await?;
    let found = proposal(&mut tx, tenant, id).await;
    tx.commit().await?;
    Ok(match found {
        Ok(p) => answer(serde_json::to_string_pretty(&p).unwrap_or_default(), false),
        Err(_) => answer(format!("there is no proposal {id}"), true),
    })
}

/// The shop's approval setting (`assistant:approve`).
#[get("/api/assistant/settings", tag = "assistant")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<AssistantSettings>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_APPROVE).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Choose whether an assistant's changes wait for approval
/// (`assistant:manage`). Publishes `assistant_settings.updated`.
#[put("/api/assistant/settings", tag = "assistant")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Json(s): Json<AssistantSettings>,
) -> Result<Json<AssistantSettings>> {
    if !matches!(s.approval.as_str(), "writes" | "off") {
        return Err(Error::unprocessable("approval must be `writes` or `off`")
            .with_code("invalid_approval"));
    }
    let mut tx = tenant.begin_for(&db, &ASSISTANT_MANAGE).await?;
    let saved = sqlx::query_as::<_, AssistantSettings>(
        "INSERT INTO assistant_settings (tenant_id, approval) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET approval = EXCLUDED.approval, updated_at = now() \
         RETURNING approval",
    )
    .bind(tenant.id())
    .bind(&s.approval)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "assistant_settings.updated", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// Changes assistants asked for, newest first (`assistant:approve`).
/// Filter with `?status=pending`, `?tool=` or `?proposed_by=`.
#[get("/api/assistant/proposals", tag = "assistant")]
async fn list_proposals(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Proposal>>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_APPROVE).await?;
    let page: Paged<Row> = TenantListing::new("assistant_proposals", COLUMNS)
        .sortable(["id"])
        .default_sort("-id")
        .filter_text("status")
        .filter_text("tool")
        .filter_text("proposed_by")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items: page.items.into_iter().map(Proposal::from).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// One proposed change (`assistant:approve`).
#[get("/api/assistant/proposals/{id}", tag = "assistant")]
async fn get_proposal(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Proposal>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_APPROVE).await?;
    let p = proposal(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Moves a pending proposal on, or 409 (`not_pending`).
async fn decide(db: &Db, tenant: &Tenant, id: &str, to: &str, event: &str) -> Result<Proposal> {
    let mut tx = tenant.begin_for(db, &ASSISTANT_APPROVE).await?;
    let id = proposal(&mut tx, tenant, id).await?.id;
    let moved = sqlx::query(
        "UPDATE assistant_proposals SET status = $3, decided_by = $4, decided_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND status = 'pending'",
    )
    .bind(tenant.id())
    .bind(&id)
    .bind(to)
    .bind(tenant.actor_id())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if moved == 0 {
        return Err(Error::conflict("this proposal was already decided").with_code("not_pending"));
    }
    let p = proposal(&mut tx, tenant, &id).await?;
    tenant.publish(&mut tx, event, &p.event()).await?;
    tx.commit().await?;
    Ok(p)
}

/// Approve a proposed change: it runs now, with your permissions, and its
/// answer is kept on the proposal (`assistant:approve`, plus whatever the
/// change itself needs). Publishes `assistant_proposal.approved`, then
/// `assistant_proposal.applied` or `assistant_proposal.failed`.
#[post("/api/assistant/proposals/{id}/approve", tag = "assistant")]
async fn approve(
    db: Db,
    tenant: Tenant,
    shared: State<Shared>,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(id): Path<String>,
) -> Result<Json<Proposal>> {
    let p = decide(&db, &tenant, &id, "approved", "assistant_proposal.approved").await?;
    let method = p
        .method
        .parse::<Method>()
        .map_err(|_| Error::internal("a proposal holds an unknown method"))?;
    // The change acts for this shop, whatever shop the approver's headers name.
    let mut headers = headers;
    headers.insert(
        soat_tenancy::HEADER,
        HeaderValue::from_str(tenant.id()).map_err(|_| Error::internal("bad shop id"))?,
    );
    let (code, out) = shared
        .api
        .send(&db, method, &p.path, p.body.as_ref(), &headers, conn)
        .await;
    let ok = (200..300).contains(&code);
    let mut tx = tenant.begin_for(&db, &ASSISTANT_APPROVE).await?;
    sqlx::query(
        "UPDATE assistant_proposals SET status = $3, result_status = $4, result = $5 \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&p.id)
    .bind(if ok { "applied" } else { "failed" })
    .bind(i32::from(code))
    .bind(SqlJson(&out))
    .execute(&mut *tx)
    .await?;
    let p = proposal(&mut tx, &tenant, &p.id).await?;
    let kind = if ok {
        "assistant_proposal.applied"
    } else {
        "assistant_proposal.failed"
    };
    tenant.publish(&mut tx, kind, &p.event()).await?;
    tx.commit().await?;
    Ok(Json(p))
}

/// Reject a proposed change; it never runs (`assistant:approve`).
/// Publishes `assistant_proposal.rejected`.
#[post("/api/assistant/proposals/{id}/reject", tag = "assistant")]
async fn reject(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Proposal>> {
    let p = decide(&db, &tenant, &id, "rejected", "assistant_proposal.rejected").await?;
    Ok(Json(p))
}
