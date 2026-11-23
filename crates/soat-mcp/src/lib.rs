//! The MCP server: Claude (or any MCP client) manages a shop.
//!
//! `POST /mcp` speaks the Model Context Protocol over HTTP (JSON-RPC,
//! answered as JSON). Every operation of the shop API is a tool, generated
//! from the OpenAPI spec, so a new endpoint is a new tool with no extra
//! work. A tool call runs as an ordinary API request, in process, with the
//! caller's credentials: an API key scoped to the permissions the shop gave
//! it, or a staff member's session. What the caller may not do, Claude may
//! not do either.
//!
//! Writes wait for a person by default: a call that would change something
//! becomes a proposal, which staff with `assistant:approve` approve (it then
//! runs with the approver's permissions) or reject. A shop can turn that
//! off (`assistant:manage`).

mod approval;
mod chat;
mod claude;
pub mod keys;
mod plan;
mod tools;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Map, Value};
use soat_core::secret::Sealer;
use soat_core::{id_kind, Permission};
use soat_module::Module;
use soat_tenancy::Tenant;
use zapline::db::sqlx::PgPool;
use zapline::db::Db;
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::test::TestClient;

pub use approval::{AssistantSettings, Proposal};
pub use tools::Tool;

id_kind!(ProposalId, ProposalKind, "prop");

/// Connect an MCP client and use the shop's tools.
pub const ASSISTANT_USE: Permission = Permission::new(
    "assistant:use",
    "Connect Claude or another MCP client and use the shop's tools",
)
.for_staff();
/// See an assistant's proposed changes and approve or reject them.
pub const ASSISTANT_APPROVE: Permission = Permission::new(
    "assistant:approve",
    "See an assistant's proposed changes, approve or reject them",
)
.for_staff();
/// Choose whether an assistant's changes wait for approval.
pub const ASSISTANT_MANAGE: Permission = Permission::new(
    "assistant:manage",
    "Choose whether an assistant's changes wait for approval, and set the shop's Anthropic key",
);

/// The MCP protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub(crate) const INSTRUCTIONS: &str =
    "Tools for running one SOAT shop: catalog, prices, stock, customers, \
orders, payments, shipping, tax, discounts, invoices and email. Each tool is one API operation; \
money is in minor units (cents) with a currency. Read before you change. A change may wait for \
a person's approval: the tool then answers with a proposal id, and proposal_status tells you \
what became of it.";

/// The requests behind tool calls: the API itself, in process.
#[derive(Clone)]
pub(crate) struct Api {
    client: TestClient,
    /// The server's database pool, lent to the API on its first request.
    pool: Arc<OnceLock<PgPool>>,
}

impl Api {
    fn lend(&self, db: &Db) {
        let _ = self.pool.get_or_init(|| db.0.clone());
    }

    /// Runs `method path` with the caller's credentials. Answers the status
    /// and the body (JSON when it is JSON).
    pub(crate) async fn send(
        &self,
        db: &Db,
        method: Method,
        path: &str,
        body: Option<&Value>,
        from: &HeaderMap,
        conn: ConnInfo,
    ) -> (u16, Value) {
        if path.parse::<Uri>().is_err() {
            return (400, json!({ "message": "not a valid path" }));
        }
        self.lend(db);
        let mut req = self.client.request(method, path);
        for name in [
            "authorization",
            soat_tenancy::HEADER,
            "host",
            "accept-language",
        ] {
            if let Some(v) = from.get(name) {
                req = req.header(name, v.clone());
            }
        }
        if let Some(addr) = conn.remote_addr {
            req = req.remote_addr(addr);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await;
        let text = res.text();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (res.status().as_u16(), body)
    }
}

/// The MCP module.
pub struct Mcp {
    api: Api,
    tools: Arc<Vec<Tool>>,
    sealer: Option<Sealer>,
    anthropic: Arc<str>,
}

/// Where Anthropic's API is, unless a test points elsewhere.
pub const ANTHROPIC_API: &str = "https://api.anthropic.com";

impl Mcp {
    /// Serves the API that `build` mounts on a fresh app (the same modules
    /// as the server's, without background jobs) as tools. That app shares
    /// the server's database pool; it never listens on a socket.
    pub fn new(build: impl FnOnce(App) -> App) -> Self {
        let pool: Arc<OnceLock<PgPool>> = Arc::default();
        let lent = pool.clone();
        let app = App::new().on_startup(move |res: Resources| async move {
            let pool = lent
                .get()
                .cloned()
                .ok_or_else(|| Error::internal("the MCP server started without a database"))?;
            res.insert(pool);
            Ok(())
        });
        let client = build(app).test();
        let spec = OpenApi::new("SOAT", env!("CARGO_PKG_VERSION")).generate(client.routes());
        Self {
            api: Api { client, pool },
            tools: Arc::new(tools::from_spec(&spec)),
            sealer: None,
            anthropic: ANTHROPIC_API.into(),
        }
    }

    /// Lets shops store their own Anthropic key, sealed with `sealer`.
    /// Without one, storing a key is a 409 `no_secret_key`.
    #[must_use]
    pub fn with_sealer(mut self, sealer: Option<Sealer>) -> Self {
        self.sealer = sealer;
        self
    }

    /// Calls Anthropic's API at `base` instead of [`ANTHROPIC_API`].
    #[must_use]
    pub fn with_anthropic_api(mut self, base: &str) -> Self {
        self.anthropic = base.into();
        self
    }

    /// The tools on offer.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }
}

#[derive(Clone)]
pub(crate) struct Shared {
    api: Api,
    tools: Arc<Vec<Tool>>,
    sealer: Option<Sealer>,
    anthropic: Arc<str>,
}

impl Module for Mcp {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[ASSISTANT_USE, ASSISTANT_APPROVE, ASSISTANT_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        let app = app
            .state(Shared {
                api: self.api.clone(),
                tools: self.tools.clone(),
                sealer: self.sealer.clone(),
                anthropic: self.anthropic.clone(),
            })
            .mount((rpc, rpc_get));
        plan::mount(chat::mount(keys::mount(approval::mount(app))))
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn failure(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool's answer as text.
fn answer(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn pretty(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The MCP endpoint (Streamable HTTP, JSON answers; `assistant:use`).
/// Send JSON-RPC: `initialize`, `ping`, `tools/list`, `tools/call`. Each API
/// operation is a tool; a call runs with the caller's own permissions, and
/// one that changes something waits for approval when the shop says so.
#[post("/mcp", tag = "assistant")]
async fn rpc(
    db: Db,
    tenant: Tenant,
    shared: State<Shared>,
    headers: HeaderMap,
    conn: ConnInfo,
    body: Bytes,
) -> Result<Response> {
    let tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    tx.commit().await?;
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(failure(&Value::Null, -32700, "not JSON")),
        )
            .into_response());
    };
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A batch, or a response to a request this server never makes.
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        if msg.is_array() || msg.get("result").is_none() && msg.get("error").is_none() {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(failure(&id, -32600, "send one JSON-RPC request")),
            )
                .into_response());
        }
        return Ok(StatusCode::ACCEPTED.into_response());
    };
    let Some(id) = msg.get("id").cloned() else {
        // Notifications (`notifications/initialized`, cancellations) need no answer.
        return Ok(StatusCode::ACCEPTED.into_response());
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let out = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            reply(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "soat", "title": "SOAT", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => reply(&id, json!({})),
        "tools/list" => {
            let mut list: Vec<Value> = shared.tools.iter().map(Tool::listing).collect();
            list.push(approval::status_tool());
            reply(&id, json!({ "tools": list }))
        }
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Ok(Json(failure(&id, -32602, "name the tool to call")).into_response());
            };
            let empty = Map::new();
            let args = params["arguments"].as_object().unwrap_or(&empty);
            if name == approval::STATUS_TOOL {
                reply(&id, approval::status(&db, &tenant, args).await?)
            } else if let Some(tool) = shared.tools.iter().find(|t| t.name == name) {
                reply(
                    &id,
                    call(&db, &tenant, &shared.api, tool, args, &headers, conn).await?,
                )
            } else {
                failure(&id, -32602, &format!("there is no tool {name}"))
            }
        }
        _ => failure(&id, -32601, &format!("{method} is not supported")),
    };
    Ok(Json(out).into_response())
}

/// This server sends no messages of its own, so there is no stream to open.
#[get("/mcp", tag = "assistant")]
async fn rpc_get() -> Result<StatusCode> {
    Err(Error::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "this MCP server answers POST requests only",
    ))
}

async fn call(
    db: &Db,
    tenant: &Tenant,
    api: &Api,
    tool: &Tool,
    args: &Map<String, Value>,
    headers: &HeaderMap,
    conn: ConnInfo,
) -> Result<Value> {
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let o = outcome(db, tenant, api, tool, args, reason, headers, conn).await?;
    Ok(answer(o.text, o.is_error))
}

/// What a tool call came to.
pub(crate) struct Outcome {
    pub text: String,
    pub is_error: bool,
    /// The proposal it became, when it waits for approval.
    pub proposal: Option<String>,
}

/// Runs `tool` with `args` as the caller, or records it as a proposal
/// (with `reason`) when it changes something and the shop wants changes
/// approved.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn outcome(
    db: &Db,
    tenant: &Tenant,
    api: &Api,
    tool: &Tool,
    args: &Map<String, Value>,
    reason: &str,
    headers: &HeaderMap,
    conn: ConnInfo,
) -> Result<Outcome> {
    let (method, path, body) = match tool.request(args) {
        Ok(r) => r,
        Err(text) => {
            return Ok(Outcome {
                text,
                is_error: true,
                proposal: None,
            })
        }
    };
    if tool.writes() && approval::required(db, tenant).await? {
        let p = approval::propose(db, tenant, tool, &method, &path, body.as_ref(), reason).await?;
        return Ok(Outcome {
            text: format!(
                "This change waits for a person's approval. Proposal {} ({} {}). \
                 Call proposal_status with its id to see what became of it.",
                p.id, p.method, p.path
            ),
            is_error: false,
            proposal: Some(p.id),
        });
    }
    let (status, out) = api
        .send(db, method, &path, body.as_ref(), headers, conn)
        .await;
    Ok(if (200..300).contains(&status) {
        Outcome {
            text: if out.is_null() || out == "" {
                format!("Done ({status}).")
            } else {
                pretty(&out)
            },
            is_error: false,
            proposal: None,
        }
    } else {
        Outcome {
            text: format!("HTTP {status}: {}", pretty(&out)),
            is_error: true,
            proposal: None,
        }
    })
}
