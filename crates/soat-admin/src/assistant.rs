//! The assistant panel: chat with Claude about the shop, with the shop's
//! own Anthropic key. Claude acts as the signed-in staff member; changes
//! it proposes wait here for approval.

use serde_json::{json, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::Fields;
use crate::{Ctx, Shared};

/// The chats the staff member started, and the one open, if any.
async fn view(mut cx: Ctx, open: Option<&str>, error: Option<String>, draft: &str) -> Response {
    let (status, list) = cx
        .call(Method::GET, "/api/assistant/chats?limit=50", None)
        .await;
    match status {
        200 => {}
        401 => return crate::to_login(),
        _ => return cx.failed(status, &list),
    }
    let mut chats = list["items"].as_array().cloned().unwrap_or_default();
    for c in &mut chats {
        c["current"] = json!(open.is_some_and(|o| c["id"] == o));
    }
    cx.set("chats", Value::Array(chats));
    if let Some(id) = open {
        let chat = page_or!(cx.get(&format!("/api/assistant/chats/{id}")).await);
        let entries = entries(&cx, &chat).await;
        cx.set("chat", chat);
        cx.set("entries", Value::Array(entries));
    }
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.set("draft", json!(draft));
    cx.set("page_title", json!("Assistant"));
    cx.render("assistant", StatusCode::OK)
}

/// The chat's entries with what the template needs: their kind as flags,
/// tool input as text, and each proposal's state.
async fn entries(cx: &Ctx, chat: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for e in chat["entries"].as_array().cloned().unwrap_or_default() {
        let mut e = e;
        let kind = e["kind"].as_str().unwrap_or_default().to_owned();
        e[kind.as_str()] = json!(true);
        if kind == "tool" {
            e["label"] = json!(e["name"].as_str().unwrap_or_default().replace('_', " "));
            e["input_text"] = json!(serde_json::to_string_pretty(&e["input"]).unwrap_or_default());
            if let Some(p) = e["proposal"].as_str().map(str::to_owned) {
                let (status, prop) = cx
                    .call(Method::GET, &format!("/api/assistant/proposals/{p}"), None)
                    .await;
                if status == 200 {
                    let state = prop["status"].as_str().unwrap_or_default().to_owned();
                    e["pending"] = json!(state == "pending");
                    e["state"] = json!(state);
                } else {
                    // Staff who may not approve see that it waits.
                    e["state"] = json!("waiting for approval");
                }
            }
        }
        out.push(e);
    }
    out
}

/// What went wrong, in words for the page.
fn problem(status: u16, body: &Value) -> String {
    match (status, body["code"].as_str()) {
        (409, Some("no_assistant_key")) => {
            "The shop has no Anthropic key yet. An owner adds one under Settings, Assistant."
                .to_owned()
        }
        (403, _) => "Your role does not allow using the assistant.".to_owned(),
        _ => body["error"]
            .as_str()
            .or(body["message"].as_str())
            .unwrap_or("Claude could not answer. Try again.")
            .to_owned(),
    }
}

/// The assistant: your chats with Claude, and a new one.
#[get("/admin/{shop}/assistant", tag = "admin")]
pub(crate) async fn index(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "assistant").await);
    Ok(view(cx, None, None, "").await)
}

/// One of your chats with Claude.
#[get("/admin/{shop}/assistant/{id}", tag = "admin")]
pub(crate) async fn show(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "assistant").await);
    Ok(view(cx, Some(&id), None, "").await)
}

/// Start a chat (a form post): Claude answers, then the chat opens.
#[post("/admin/{shop}/assistant", tag = "admin")]
pub(crate) async fn start(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "assistant").await);
    let message = Fields::parse(&body).get("message");
    let (status, out) = cx
        .call(
            Method::POST,
            "/api/assistant/chats",
            Some(&json!({ "message": message })),
        )
        .await;
    Ok(match status {
        201 => Redirect::to(&format!(
            "/admin/{shop}/assistant/{}",
            out["id"].as_str().unwrap_or_default()
        ))
        .into_response(),
        401 => crate::to_login(),
        _ => view(cx, None, Some(problem(status, &out)), &message).await,
    })
}

/// Send Claude a message in a chat (a form post).
#[post("/admin/{shop}/assistant/{id}", tag = "admin")]
pub(crate) async fn send(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "assistant").await);
    let message = Fields::parse(&body).get("message");
    let (status, out) = cx
        .call(
            Method::POST,
            &format!("/api/assistant/chats/{id}/messages"),
            Some(&json!({ "message": message })),
        )
        .await;
    Ok(match status {
        200 if out["note"].is_string() => {
            let note = out["note"].as_str().map(str::to_owned);
            view(cx, Some(&id), note, "").await
        }
        200 => Redirect::to(&format!("/admin/{shop}/assistant/{id}")).into_response(),
        401 => crate::to_login(),
        404 => cx.failed(404, &out),
        _ => view(cx, Some(&id), Some(problem(status, &out)), &message).await,
    })
}

/// Delete a chat, or approve or reject a change Claude proposed in it
/// (form posts).
#[post("/admin/{shop}/assistant/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "assistant").await);
    let proposal = Fields::parse(&body).get("proposal");
    let (method, path, back) = match action.as_str() {
        "delete" => (
            Method::DELETE,
            format!("/api/assistant/chats/{id}"),
            format!("/admin/{shop}/assistant"),
        ),
        "approve" | "reject" if proposal.starts_with("prop_") => (
            Method::POST,
            format!("/api/assistant/proposals/{proposal}/{action}"),
            format!("/admin/{shop}/assistant/{id}"),
        ),
        _ => return Ok(cx.failed(404, &Value::Null)),
    };
    let (status, out) = cx.call(method, &path, None).await;
    Ok(match status {
        200..=299 => Redirect::to(&back).into_response(),
        401 => crate::to_login(),
        _ => cx.failed(status, &out),
    })
}
