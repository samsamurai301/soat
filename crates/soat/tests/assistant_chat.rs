//! H7: staff chat with Claude about their shop, with the shop's own key;
//! Claude's tools run as the caller, and changes wait for approval.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use soat_testing::{fake_api, Log};
use zapline::db::Database;
use zapline::test::TestClient;

const SHOP_KEY: &str = "sk-ant-api03-test-key-0123456789";

/// The app with Claude's API faked: each request gets the next answer.
async fn app(answers: Vec<(u16, Value)>) -> (TestClient, String, Log) {
    let answers = Arc::new(answers);
    let next = Arc::new(AtomicUsize::new(0));
    let (base, log) = fake_api(move |_| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        answers
            .get(n)
            .cloned()
            .unwrap_or((500, json!({ "error": { "message": "no more answers" } })))
    })
    .await;
    let mut settings = soat::Settings::for_tests(common::SETUP_TOKEN);
    settings.anthropic_api = base;
    let url = common::fresh_database_url().await;
    let client = soat::app(Database::postgres(url), settings).test();
    let admin = common::setup_admin(&client).await;
    common::create_tenants(&client, &admin, &["acme", "globex"]).await;
    (client, admin, log)
}

async fn key(client: &TestClient, admin: &str, permissions: &[&str]) -> String {
    let k: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(admin)
        .json(&json!({ "name": "staff app", "permissions": permissions }))
        .send()
        .await
        .assert_status(201)
        .json();
    k["secret"].as_str().unwrap().to_owned()
}

async fn set_shop_key(client: &TestClient, admin: &str) {
    client
        .put("/api/assistant/key")
        .header("x-soat-tenant", "acme")
        .bearer(admin)
        .json(&json!({ "api_key": SHOP_KEY, "model": "claude-test" }))
        .send()
        .await
        .assert_status(200);
}

fn message(stop: &str, content: Value) -> (u16, Value) {
    (
        200,
        json!({ "id": "msg_1", "type": "message", "role": "assistant",
                "content": content, "stop_reason": stop }),
    )
}

#[tokio::test]
async fn staff_chat_with_claude_and_changes_wait() {
    let (client, admin, log) = app(vec![
        message(
            "tool_use",
            json!([
                { "type": "thinking", "thinking": "", "signature": "sig" },
                { "type": "text", "text": "I'll check the catalog, then add the shoe." },
                { "type": "tool_use", "id": "toolu_1", "name": "list_products", "input": {} },
                { "type": "tool_use", "id": "toolu_2", "name": "create_product", "input": {
                    "body": { "sku": "SHOE", "title": "Shoe", "price_minor": 4999, "currency": "EUR" } } }
            ]),
        ),
        message(
            "end_turn",
            json!([{ "type": "text", "text": "The shop has no products. Adding the shoe waits for your approval." }]),
        ),
        message(
            "end_turn",
            json!([{ "type": "text", "text": "Approve it under Settings, Assistant." }]),
        ),
        message("refusal", json!([])),
    ])
    .await;
    let me = key(
        &client,
        &admin,
        &["assistant:use", "products:read", "products:write"],
    )
    .await;
    let say = |path: String, text: &'static str| {
        client
            .post(&path)
            .header("x-soat-tenant", "acme")
            .bearer(&me)
            .json(&json!({ "message": text }))
            .send()
    };

    // No key yet: nothing to call Claude with.
    say("/api/assistant/chats".into(), "Add a shoe")
        .await
        .assert_status(409);
    assert!(log.lock().unwrap().is_empty());
    set_shop_key(&client, &admin).await;

    let res = say("/api/assistant/chats".into(), "Add a shoe for 49.99 EUR").await;
    res.assert_status(201);
    let chat: Value = res.json();
    let id = chat["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("chat_"));
    assert_eq!(chat["title"], "Add a shoe for 49.99 EUR");
    let entries = chat["entries"].as_array().unwrap();
    assert_eq!(entries[0]["kind"], "user");
    assert_eq!(entries[1]["kind"], "assistant");
    assert_eq!(entries[2]["name"], "list_products");
    assert_eq!(entries[2]["error"], false);
    assert_eq!(entries[3]["name"], "create_product");
    let proposal = entries[3]["proposal"].as_str().unwrap().to_owned();
    assert!(proposal.starts_with("prop_"));
    assert!(entries[4]["text"]
        .as_str()
        .unwrap()
        .contains("waits for your approval"));
    assert_eq!(entries.len(), 5);

    // What Claude received: the shop's key and model, tools found by search.
    {
        let seen = log.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let first = &seen[0];
        assert_eq!(first.path, "/v1/messages");
        assert_eq!(first.header("x-api-key"), Some(SHOP_KEY));
        assert!(first.header("anthropic-version").is_some());
        let body = first.json();
        assert_eq!(body["model"], "claude-test");
        assert!(body["system"].as_str().unwrap().contains("`acme`"));
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "tool_search_tool_bm25_20251119");
        let create = tools
            .iter()
            .find(|t| t["name"] == "create_product")
            .unwrap();
        assert_eq!(create["defer_loading"], true);
        assert!(!tools.iter().any(|t| t["name"] == "start_chat"));
        // The second call replays the first answer, thinking included, and
        // answers both tool calls.
        let second = seen[1].json();
        let msgs = second["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][0]["signature"], "sig");
        let results = msgs[2]["content"].as_array().unwrap();
        assert_eq!(results[0]["tool_use_id"], "toolu_1");
        assert!(results[0]["content"]
            .as_str()
            .unwrap()
            .contains("\"items\""));
        assert!(results[1]["content"].as_str().unwrap().contains(&proposal));
    }

    // Nothing changed yet; the proposal names the reason Claude gave.
    let p: Value = client
        .get(&format!("/api/assistant/proposals/{proposal}"))
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(p["status"], "pending");
    assert!(p["reason"].as_str().unwrap().contains("add the shoe"));

    // The chat goes on; its system prompt is replayed as it began.
    let res = say(
        format!("/api/assistant/chats/{id}/messages"),
        "Where do I approve it?",
    )
    .await;
    res.assert_status(200);
    let chat: Value = res.json();
    assert_eq!(chat["entries"].as_array().unwrap().len(), 7);
    {
        let seen = log.lock().unwrap();
        assert_eq!(seen[2].json()["system"], seen[0].json()["system"]);
        assert_eq!(seen[2].json()["messages"].as_array().unwrap().len(), 5);
    }

    // A refusal keeps nothing of the turn.
    let res = say(
        format!("/api/assistant/chats/{id}/messages"),
        "Something else",
    )
    .await;
    res.assert_status(422);
    assert_eq!(res.json::<Value>()["code"], "refused");
    let chat: Value = client
        .get(&format!("/api/assistant/chats/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&me)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(chat["entries"].as_array().unwrap().len(), 7);

    // Only who started a chat sees it.
    let list: Value = client
        .get("/api/assistant/chats")
        .header("x-soat-tenant", "acme")
        .bearer(&me)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(list["total"], 1);
    for other in [&admin, &key(&client, &admin, &["assistant:use"]).await] {
        let list: Value = client
            .get("/api/assistant/chats")
            .header("x-soat-tenant", "acme")
            .bearer(other)
            .send()
            .await
            .assert_status(200)
            .json();
        assert_eq!(list["total"], 0);
        client
            .get(&format!("/api/assistant/chats/{id}"))
            .header("x-soat-tenant", "acme")
            .bearer(other)
            .send()
            .await
            .assert_status(404);
    }
    client
        .get(&format!("/api/assistant/chats/{id}"))
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);

    // Without `assistant:use`, no chat.
    let reader = key(&client, &admin, &["products:read"]).await;
    client
        .get("/api/assistant/chats")
        .header("x-soat-tenant", "acme")
        .bearer(&reader)
        .send()
        .await
        .assert_status(403);

    client
        .delete(&format!("/api/assistant/chats/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&me)
        .send()
        .await
        .assert_status(204);
    client
        .get(&format!("/api/assistant/chats/{id}"))
        .header("x-soat-tenant", "acme")
        .bearer(&me)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn claude_errors_are_explained() {
    let (client, admin, _log) = app(vec![
        (
            401,
            json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } }),
        ),
        (
            529,
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
        ),
        message(
            "max_tokens",
            json!([{ "type": "tool_use", "id": "toolu_9", "name": "list_products", "input": {} }]),
        ),
    ])
    .await;
    set_shop_key(&client, &admin).await;
    let start = || {
        client
            .post("/api/assistant/chats")
            .header("x-soat-tenant", "acme")
            .bearer(&admin)
            .json(&json!({ "message": "Hello" }))
            .send()
    };
    let res = start().await;
    res.assert_status(422);
    assert_eq!(res.json::<Value>()["code"], "assistant_key_rejected");
    start().await.assert_status(503);
    // A cut-off tool call never runs.
    let res = start().await;
    res.assert_status(201);
    let chat: Value = res.json();
    assert!(chat["note"].as_str().unwrap().contains("cut off"));
    let entries = chat["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1]["kind"], "assistant");
    let list: Value = client
        .get("/api/assistant/chats")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    assert_eq!(list["total"], 1);
}

#[tokio::test]
async fn staff_chat_in_the_admin_and_approve_there() {
    let (client, admin, _log) = app(vec![
        message(
            "tool_use",
            json!([
                { "type": "text", "text": "Adding the shoe." },
                { "type": "tool_use", "id": "toolu_1", "name": "create_product", "input": {
                    "body": { "sku": "SHOE", "title": "Trail shoe", "price_minor": 4999, "currency": "EUR" } } }
            ]),
        ),
        message(
            "end_turn",
            json!([{ "type": "text", "text": "It waits for your approval." }]),
        ),
    ])
    .await;
    let res = client
        .post("/admin/login")
        .form(&json!({ "email": common::ADMIN_EMAIL, "password": common::ADMIN_PASSWORD }))
        .send()
        .await;
    res.assert_status(303);
    let cookie = format!("soat_admin={}", res.cookie("soat_admin").unwrap().value());
    let post = |path: String, form: Value| {
        client
            .post(&path)
            .header("cookie", &cookie)
            .form(&form)
            .send()
    };

    let page = client
        .get("/admin/acme/assistant")
        .header("cookie", &cookie)
        .send()
        .await;
    page.assert_status(200);
    assert!(page.text().contains("Ask Claude about your shop"));

    // Without the shop's key, the page says what to do and keeps the draft.
    let res = post(
        "/admin/acme/assistant".into(),
        json!({ "message": "Add a trail shoe" }),
    )
    .await;
    res.assert_status(200);
    let html = res.text();
    assert!(html.contains("no Anthropic key yet"), "{html}");
    assert!(html.contains("Add a trail shoe</textarea>"));

    set_shop_key(&client, &admin).await;
    let res = post(
        "/admin/acme/assistant".into(),
        json!({ "message": "Add a trail shoe" }),
    )
    .await;
    res.assert_status(303);
    let at = res.header("location").unwrap().to_owned();
    assert!(at.starts_with("/admin/acme/assistant/chat_"));
    let html = client
        .get(&at)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("Adding the shoe."));
    assert!(html.contains("create product"));
    assert!(html.contains("It waits for your approval."));
    let proposal = html
        .split("name=\"proposal\" value=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap()
        .to_owned();
    assert!(proposal.starts_with("prop_"));

    let res = post(format!("{at}/approve"), json!({ "proposal": proposal })).await;
    res.assert_status(303);
    assert_eq!(res.header("location"), Some(at.as_str()));
    let html = client
        .get(&at)
        .header("cookie", &cookie)
        .send()
        .await
        .text();
    assert!(html.contains("applied"));
    assert!(!html.contains("name=\"proposal\""));
    let products: Value = client
        .get("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    assert_eq!(products["items"][0]["title"], "Trail shoe");

    post(format!("{at}/delete"), json!({}))
        .await
        .assert_status(303);
    client
        .get(&at)
        .header("cookie", &cookie)
        .send()
        .await
        .assert_status(404);
}
