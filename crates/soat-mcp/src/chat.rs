//! The assistant in the admin: staff chat with Claude about their shop.
//! SOAT calls Claude with the shop's own Anthropic key and offers it the
//! same tools as the MCP server, found through tool search. Tool calls run
//! as the staff member, and a change waits for approval when the shop says
//! so, exactly as over MCP.
//!
//! A chat keeps its messages as Claude sent them, thinking included, and
//! its system prompt as it began, so later turns replay them unchanged.

use serde_json::{json, Map, Value};
use soat_tenancy::{Tenant, TenantListing};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{self, PgConnection};
use zapline::db::{Db, ListParams, Paged};
use zapline::prelude::*;

use crate::keys::{anthropic_key, AnthropicKey};
use crate::{approval, claude, outcome, Shared, Tool, ASSISTANT_USE, INSTRUCTIONS};

soat_core::id_kind!(ChatId, ChatKind, "chat");

/// Model calls one message may take (tool rounds included).
const MAX_STEPS: usize = 12;
/// Output tokens per model call.
const MAX_TOKENS: u32 = 16_000;
/// A tool's answer is cut to this many characters before Claude reads it.
const RESULT_CHARS: usize = 30_000;
/// Messages a chat may hold before staff start a new one.
const MAX_MESSAGES: usize = 400;

pub(crate) fn mount(app: App) -> App {
    app.mount((list_chats, start_chat, get_chat, send_message, delete_chat))
}

/// A chat in a list.
#[model]
#[derive(sqlx::FromRow)]
pub struct ChatSummary {
    /// e.g. `chat_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// The first message, shortened.
    pub title: String,
    /// RFC 3339, UTC.
    pub created_at: String,
    pub updated_at: String,
}

/// A chat as people read it.
#[model]
pub struct ChatView {
    pub id: String,
    pub title: String,
    /// In order: `user` and `assistant` text, and `tool` calls with their
    /// `name`, `input`, a short `result`, `error`, and the `proposal` the
    /// call became when it waits for approval.
    pub entries: Vec<Value>,
    /// Why Claude stopped early on the last message, if it did.
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A message to Claude.
#[model]
pub struct ChatMessage {
    #[validate(length(min = 1, max = 8000))]
    pub message: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    title: String,
    system: String,
    messages: SqlJson<Vec<Value>>,
    proposals: SqlJson<Map<String, Value>>,
    created_at: String,
    updated_at: String,
}

const TIMES: &str = "to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

const LIST_COLUMNS: &str = "id, title, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

impl Row {
    fn view(&self, note: Option<String>) -> ChatView {
        ChatView {
            id: self.id.clone(),
            title: self.title.clone(),
            entries: entries(&self.messages, &self.proposals),
            note,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

fn actor(tenant: &Tenant) -> Result<String> {
    tenant
        .actor_id()
        .map(str::to_owned)
        .ok_or_else(|| Error::internal("no actor after the permission check"))
}

/// The caller's own chat `id`, or 404.
async fn load(conn: &mut PgConnection, tenant: &Tenant, id: &str) -> Result<Row> {
    let id = ChatId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Row>(&format!(
        "SELECT id, title, system, messages, proposals, {TIMES} FROM assistant_chats \
         WHERE tenant_id = $1 AND id = $2 AND started_by = $3"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(actor(tenant)?)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's key, or 409 `no_assistant_key`.
async fn key(conn: &mut PgConnection, tenant: &Tenant, s: &Shared) -> Result<AnthropicKey> {
    let missing = || {
        Error::conflict(
            "the shop has no Anthropic key: an owner adds one in the assistant settings",
        )
        .with_code("no_assistant_key")
    };
    let sealer = s.sealer.as_ref().ok_or_else(missing)?;
    anthropic_key(conn, tenant, sealer)
        .await?
        .ok_or_else(missing)
}

/// The caller's chats, newest first (`assistant:use`). Only the staff
/// member or API key that started a chat sees it.
#[get("/api/assistant/chats", tag = "assistant")]
async fn list_chats(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<ChatSummary>>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let page = TenantListing::new("assistant_chats", LIST_COLUMNS)
        .sortable(["id"])
        .default_sort("-id")
        .search(["title"])
        .scope_text("started_by", actor(&tenant)?)
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Start a chat with Claude and send its first message (`assistant:use`).
/// Claude answers with the shop's own Anthropic key (409
/// `no_assistant_key` without one) and may use any tool its caller could;
/// changes wait for approval when the shop says so. Publishes
/// `assistant_chat.created`.
#[post("/api/assistant/chats", tag = "assistant", status = 201)]
async fn start_chat(
    db: Db,
    tenant: Tenant,
    s: State<Shared>,
    headers: HeaderMap,
    conn: ConnInfo,
    Valid(Json(m)): Valid<Json<ChatMessage>>,
) -> Result<(StatusCode, Json<ChatView>)> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let key = key(&mut tx, &tenant, &s).await?;
    let (today,): (String,) =
        sqlx::query_as("SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD')")
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    let system = system_prompt(&tenant, &today);
    let mut messages = Vec::new();
    let mut proposals = Map::new();
    push_user(&mut messages, m.message.trim());
    let turn = Turn {
        db: &db,
        tenant: &tenant,
        s: &s,
        key: &key,
        headers: &headers,
        conn,
    };
    let note = turn.run(&system, &mut messages, &mut proposals).await?;
    let title: String = m.message.trim().chars().take(80).collect();
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let id = ChatId::new();
    sqlx::query(
        "INSERT INTO assistant_chats (id, tenant_id, started_by, title, system, messages, \
         proposals) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_str())
    .bind(tenant.id())
    .bind(actor(&tenant)?)
    .bind(&title)
    .bind(&system)
    .bind(SqlJson(&messages))
    .bind(SqlJson(&proposals))
    .execute(&mut *tx)
    .await?;
    let row = load(&mut tx, &tenant, id.as_str()).await?;
    tenant
        .publish(
            &mut tx,
            "assistant_chat.created",
            &json!({ "id": row.id, "messages": row.messages.len() }),
        )
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row.view(note))))
}

/// One of the caller's chats (`assistant:use`).
#[get("/api/assistant/chats/{id}", tag = "assistant")]
async fn get_chat(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<ChatView>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let row = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(row.view(None)))
}

/// Send Claude another message in a chat and get its answer
/// (`assistant:use`). 409 `chat_full` once a chat is long: start a new one.
/// Publishes `assistant_chat.updated`.
#[post("/api/assistant/chats/{id}/messages", tag = "assistant")]
async fn send_message(
    db: Db,
    tenant: Tenant,
    s: State<Shared>,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(id): Path<String>,
    Valid(Json(m)): Valid<Json<ChatMessage>>,
) -> Result<Json<ChatView>> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let row = load(&mut tx, &tenant, &id).await?;
    let key = key(&mut tx, &tenant, &s).await?;
    tx.commit().await?;
    if row.messages.len() >= MAX_MESSAGES {
        return Err(Error::conflict("this chat is long: start a new one").with_code("chat_full"));
    }
    let mut messages = row.messages.0.clone();
    let mut proposals = row.proposals.0.clone();
    push_user(&mut messages, m.message.trim());
    let turn = Turn {
        db: &db,
        tenant: &tenant,
        s: &s,
        key: &key,
        headers: &headers,
        conn,
    };
    let note = turn.run(&row.system, &mut messages, &mut proposals).await?;
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    sqlx::query(
        "UPDATE assistant_chats SET messages = $3, proposals = $4, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&row.id)
    .bind(SqlJson(&messages))
    .bind(SqlJson(&proposals))
    .execute(&mut *tx)
    .await?;
    let row = load(&mut tx, &tenant, &row.id).await?;
    tenant
        .publish(
            &mut tx,
            "assistant_chat.updated",
            &json!({ "id": row.id, "messages": row.messages.len() }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(row.view(note)))
}

/// Delete one of the caller's chats (`assistant:use`). Proposals it made
/// stay. Publishes `assistant_chat.deleted`.
#[delete("/api/assistant/chats/{id}", tag = "assistant", status = 204)]
async fn delete_chat(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &ASSISTANT_USE).await?;
    let row = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM assistant_chats WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "assistant_chat.deleted", &json!({ "id": row.id }))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn system_prompt(tenant: &Tenant, today: &str) -> String {
    format!(
        "You are the assistant in the admin of the shop `{}`, talking with a member of its \
         staff. Today is {today} (UTC).\n\n{INSTRUCTIONS}\n\nThe shop's tools are not all \
         loaded: search for the one you need with tool search, by what it does (\"list \
         orders\", \"update product\"). Look things up before you answer questions about \
         the shop, and say what you did. When a change waits for approval, tell the staff \
         member it is waiting and what it will do; they approve it in the admin. Answer in \
         the language the staff member writes in, briefly, in plain text with short \
         paragraphs or lists.",
        tenant.id()
    )
}

/// Adds the staff member's words, after any tool results that end the
/// conversation so far.
fn push_user(messages: &mut Vec<Value>, text: &str) {
    let block = json!({ "type": "text", "text": text });
    if let Some(last) = messages.last_mut() {
        if last["role"] == "user" {
            if let Some(content) = last["content"].as_array_mut() {
                content.push(block);
                return;
            }
        }
    }
    messages.push(json!({ "role": "user", "content": [block] }));
}

/// Adds Claude's content, continuing its message after a pause.
fn push_assistant(messages: &mut Vec<Value>, content: Vec<Value>) {
    if content.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last["role"] == "assistant" {
            if let Some(blocks) = last["content"].as_array_mut() {
                blocks.extend(content);
                return;
            }
        }
    }
    messages.push(json!({ "role": "assistant", "content": content }));
}

/// The tools as Claude's request lists them: tool search, the proposal
/// status tool, and every API tool loaded only when searched for.
pub(crate) fn claude_tools(tools: &[Tool]) -> Vec<Value> {
    let status = approval::status_tool();
    let mut out = vec![
        json!({ "type": "tool_search_tool_bm25_20251119", "name": "tool_search_tool_bm25" }),
        json!({
            "name": status["name"],
            "description": status["description"],
            "input_schema": status["inputSchema"],
        }),
    ];
    out.extend(tools.iter().map(|t| {
        json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
            "defer_loading": true,
        })
    }));
    out
}

/// One message's work: model calls and tool rounds until Claude is done.
struct Turn<'a> {
    db: &'a Db,
    tenant: &'a Tenant,
    s: &'a Shared,
    key: &'a AnthropicKey,
    headers: &'a HeaderMap,
    conn: ConnInfo,
}

impl Turn<'_> {
    /// Runs Claude on `messages` until it answers, appending what it says
    /// and what its tools answer. Returns a note when it stopped early.
    /// A refusal is a 422 `refused`, and nothing of the turn is kept.
    async fn run(
        &self,
        system: &str,
        messages: &mut Vec<Value>,
        proposals: &mut Map<String, Value>,
    ) -> Result<Option<String>> {
        let tools = claude_tools(&self.s.tools);
        for _ in 0..MAX_STEPS {
            let body = json!({
                "model": self.key.model,
                "max_tokens": MAX_TOKENS,
                "system": system,
                "thinking": { "type": "adaptive" },
                "cache_control": { "type": "ephemeral" },
                "tools": tools,
                "messages": messages,
            });
            let out = claude::messages(&self.s.anthropic, self.key, &body).await?;
            let content = out["content"].as_array().cloned().unwrap_or_default();
            match out["stop_reason"].as_str().unwrap_or_default() {
                "refusal" => {
                    return Err(Error::unprocessable(
                        "Claude declined to help with this. Try asking another way.",
                    )
                    .with_code("refused"))
                }
                "pause_turn" => push_assistant(messages, content),
                "max_tokens" => {
                    // A cut-off tool call cannot run, nor stay unanswered.
                    let mut kept: Vec<Value> = content
                        .into_iter()
                        .filter(|b| b["type"] != "tool_use")
                        .collect();
                    if !kept.iter().any(|b| b["type"] == "text") {
                        kept.push(json!({ "type": "text", "text": "(My answer was cut off.)" }));
                    }
                    push_assistant(messages, kept);
                    return Ok(Some(
                        "Claude's answer was too long and was cut off.".to_owned(),
                    ));
                }
                "tool_use" => {
                    let reason = said(&content);
                    let calls: Vec<Value> = content
                        .iter()
                        .filter(|b| b["type"] == "tool_use")
                        .cloned()
                        .collect();
                    push_assistant(messages, content);
                    let mut results = Vec::new();
                    for c in calls {
                        let id = c["id"].as_str().unwrap_or_default().to_owned();
                        let (text, is_error, proposal) = self.tool(&c, &reason).await?;
                        if let Some(p) = proposal {
                            proposals.insert(id.clone(), json!(p));
                        }
                        results.push(json!({
                            "type": "tool_result", "tool_use_id": id,
                            "content": text, "is_error": is_error,
                        }));
                    }
                    messages.push(json!({ "role": "user", "content": results }));
                }
                _ => {
                    push_assistant(messages, content);
                    return Ok(None);
                }
            }
        }
        Ok(Some(format!(
            "Claude stopped after {MAX_STEPS} steps. Say \"go on\" to let it continue."
        )))
    }

    /// Runs one tool call: its answer, whether it failed, and the proposal
    /// it became.
    async fn tool(&self, call: &Value, reason: &str) -> Result<(String, bool, Option<String>)> {
        let name = call["name"].as_str().unwrap_or_default();
        let empty = Map::new();
        let args = call["input"].as_object().unwrap_or(&empty);
        if name == approval::STATUS_TOOL {
            let a = approval::status(self.db, self.tenant, args).await?;
            let text = a["content"][0]["text"].as_str().unwrap_or_default();
            return Ok((cut(text), a["isError"] == true, None));
        }
        let Some(tool) = self.s.tools.iter().find(|t| t.name == name) else {
            return Ok((format!("there is no tool {name}"), true, None));
        };
        let o = outcome(
            self.db,
            self.tenant,
            &self.s.api,
            tool,
            args,
            reason,
            self.headers,
            self.conn,
        )
        .await?;
        Ok((cut(&o.text), o.is_error, o.proposal))
    }
}

/// What Claude wrote in `content`, for a proposal's reason.
fn said(content: &[Value]) -> String {
    let text: Vec<&str> = content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    text.join("\n").chars().take(2000).collect()
}

fn cut(text: &str) -> String {
    if text.chars().count() <= RESULT_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(RESULT_CHARS).collect();
    format!("{head}\n… (cut: ask for less, with filters or a smaller limit)")
}

fn short(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(n).collect::<String>())
    }
}

/// The chat as people read it: text, and tool calls with their answers.
/// Thinking and tool search stay out.
fn entries(messages: &[Value], proposals: &Map<String, Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let blocks = match &m["content"] {
            Value::String(s) => vec![json!({ "type": "text", "text": s })],
            Value::Array(b) => b.clone(),
            _ => continue,
        };
        let role = m["role"].as_str().unwrap_or_default();
        for b in blocks {
            match (role, b["type"].as_str().unwrap_or_default()) {
                (_, "text") => {
                    let text = b["text"].as_str().unwrap_or_default().trim();
                    if text.is_empty() {
                        continue;
                    }
                    let kind = if role == "user" { "user" } else { "assistant" };
                    match out.last_mut() {
                        Some(last) if last["kind"] == kind => {
                            let joined =
                                format!("{}\n\n{text}", last["text"].as_str().unwrap_or(""));
                            last["text"] = json!(joined);
                        }
                        _ => out.push(json!({ "kind": kind, "text": text })),
                    }
                }
                ("assistant", "tool_use") => {
                    let id = b["id"].as_str().unwrap_or_default();
                    out.push(json!({
                        "kind": "tool", "id": id, "name": b["name"], "input": b["input"],
                        "result": null, "error": false,
                        "proposal": proposals.get(id).cloned().unwrap_or(Value::Null),
                    }));
                }
                ("user", "tool_result") => {
                    let id = b["tool_use_id"].as_str().unwrap_or_default();
                    let text = match &b["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => String::new(),
                    };
                    if let Some(e) = out
                        .iter_mut()
                        .rev()
                        .find(|e| e["kind"] == "tool" && e["id"] == id)
                    {
                        e["result"] = json!(short(&text, 600));
                        e["error"] = json!(b["is_error"] == true);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_and_text_share_a_user_message() {
        let mut m = vec![json!({ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": "ok" }] })];
        push_user(&mut m, "go on");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["content"][1]["text"], "go on");
        push_assistant(&mut m, vec![json!({ "type": "text", "text": "a" })]);
        push_assistant(&mut m, vec![json!({ "type": "text", "text": "b" })]);
        push_assistant(&mut m, vec![]);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1]["content"].as_array().unwrap().len(), 2);
        push_user(&mut m, "next");
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn entries_show_text_and_tools() {
        let messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "Raise the tee" }] }),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "", "signature": "x" },
                { "type": "text", "text": "Let me look." },
                { "type": "server_tool_use", "id": "s1", "name": "tool_search_tool_bm25", "input": {} },
                { "type": "tool_use", "id": "t1", "name": "update_product", "input": { "id": "prod_1" } }] }),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "waits", "is_error": false }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "It waits." }] }),
        ];
        let mut proposals = Map::new();
        proposals.insert("t1".into(), json!("prop_1"));
        let e = entries(&messages, &proposals);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0]["kind"], "user");
        assert_eq!(e[1]["text"], "Let me look.");
        assert_eq!(e[2]["name"], "update_product");
        assert_eq!(e[2]["result"], "waits");
        assert_eq!(e[2]["proposal"], "prop_1");
        assert_eq!(e[3]["text"], "It waits.");
        assert!(cut(&"x".repeat(RESULT_CHARS + 5)).ends_with("smaller limit)"));
    }
}
