//! H1–H3: Claude runs a shop over MCP, with scoped keys and approvals.

mod common;

use serde_json::{json, Value};
use zapline::test::TestClient;

async fn key(client: &TestClient, admin: &str, permissions: &[&str]) -> String {
    let k: Value = client
        .post("/api/api-keys")
        .header("x-soat-tenant", "acme")
        .bearer(admin)
        .json(&json!({ "name": "claude", "permissions": permissions }))
        .send()
        .await
        .assert_status(201)
        .json();
    k["secret"].as_str().unwrap().to_owned()
}

async fn rpc(client: &TestClient, secret: &str, method: &str, params: Value) -> Value {
    client
        .post("/mcp")
        .bearer(secret)
        .json(&json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }))
        .send()
        .await
        .assert_status(200)
        .json()
}

/// Calls a tool; returns its text and whether it is an error.
async fn call(client: &TestClient, secret: &str, tool: &str, args: Value) -> (String, bool) {
    let r = rpc(
        client,
        secret,
        "tools/call",
        json!({ "name": tool, "arguments": args }),
    )
    .await;
    assert_eq!(r["id"], 7, "{r}");
    let result = &r["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_owned(),
        result["isError"] == true,
    )
}

fn shoe() -> Value {
    json!({ "sku": "SHOE", "title": "Shoe", "price_minor": 4999, "currency": "EUR" })
}

async fn products(client: &TestClient, admin: &str) -> Value {
    client
        .get("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(admin)
        .send()
        .await
        .assert_status(200)
        .json()
}

async fn admin_post(client: &TestClient, admin: &str, path: &str, status: u16) -> Value {
    let res = client
        .post(path)
        .header("x-soat-tenant", "acme")
        .bearer(admin)
        .send()
        .await;
    res.assert_status(status);
    if status < 300 {
        res.json()
    } else {
        Value::Null
    }
}

#[tokio::test]
async fn claude_reads_freely_and_writes_with_approval() {
    let (client, admin) = common::with_tenants(&["acme", "globex"]).await;
    let secret = key(
        &client,
        &admin,
        &["assistant:use", "products:read", "products:write"],
    )
    .await;

    let init = rpc(
        &client,
        &secret,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "claude", "version": "1" } }),
    )
    .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "soat");
    client
        .post("/mcp")
        .bearer(&secret)
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .send()
        .await
        .assert_status(202);

    // A tool per API operation, but none for keys, sign-in or the storefront.
    let list = rpc(&client, &secret, "tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for name in [
        "create_product",
        "list_products",
        "list_orders",
        "refund_order",
        "proposal_status",
    ] {
        assert!(names.contains(&name), "{name} missing");
    }
    for name in [
        "create_key",
        "routes_login",
        "create_cart",
        "setup",
        "upload",
    ] {
        assert!(!names.contains(&name), "{name} offered");
    }
    assert!(tools.len() > 120, "{}", tools.len());
    let create = tools
        .iter()
        .find(|t| t["name"] == "create_product")
        .unwrap();
    assert_eq!(create["annotations"]["readOnlyHint"], false);
    assert!(create["inputSchema"]["$defs"]["NewProduct"].is_object());

    // Reads run at once.
    let (text, err) = call(&client, &secret, "list_products", json!({})).await;
    assert!(!err, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);

    // Writes wait for a person.
    let (text, err) = call(
        &client,
        &secret,
        "create_product",
        json!({ "body": shoe(), "reason": "the owner asked for shoes" }),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("waits for a person's approval"), "{text}");
    assert_eq!(products(&client, &admin).await["total"], 0);

    let pending: Value = client
        .get("/api/assistant/proposals?status=pending")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(pending["total"], 1);
    let p = &pending["items"][0];
    let id = p["id"].as_str().unwrap().to_owned();
    assert_eq!(p["tool"], "create_product");
    assert_eq!(p["method"], "POST");
    assert_eq!(p["path"], "/api/products");
    assert_eq!(p["body"]["sku"], "SHOE");
    assert_eq!(p["reason"], "the owner asked for shoes");
    assert!(p["proposed_by"].as_str().unwrap().starts_with("key_"));

    // Another shop never sees it.
    client
        .get(&format!("/api/assistant/proposals/{id}"))
        .header("x-soat-tenant", "globex")
        .bearer(&admin)
        .send()
        .await
        .assert_status(404);

    let done = admin_post(
        &client,
        &admin,
        &format!("/api/assistant/proposals/{id}/approve"),
        200,
    )
    .await;
    assert_eq!(done["status"], "applied");
    assert_eq!(done["result_status"], 201);
    assert_eq!(done["result"]["sku"], "SHOE");
    assert!(done["decided_by"].as_str().unwrap().starts_with("user_"));
    assert_eq!(products(&client, &admin).await["total"], 1);
    admin_post(
        &client,
        &admin,
        &format!("/api/assistant/proposals/{id}/approve"),
        409,
    )
    .await;

    let (text, err) = call(&client, &secret, "proposal_status", json!({ "id": id })).await;
    assert!(!err);
    assert!(text.contains("\"applied\""), "{text}");

    // A rejected change never runs.
    call(
        &client,
        &secret,
        "create_product",
        json!({ "body": { "sku": "HAT", "title": "Hat", "price_minor": 1, "currency": "EUR" } }),
    )
    .await;
    let pending: Value = client
        .get("/api/assistant/proposals?status=pending")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .json();
    let hat = pending["items"][0]["id"].as_str().unwrap().to_owned();
    let r = admin_post(
        &client,
        &admin,
        &format!("/api/assistant/proposals/{hat}/reject"),
        200,
    )
    .await;
    assert_eq!(r["status"], "rejected");
    assert_eq!(products(&client, &admin).await["total"], 1);

    // The event log tells who proposed and who decided.
    let events: Value = client
        .get("/api/events?kind=assistant_proposal.applied")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(events["total"], 1);
    assert!(events["items"][0]["payload"].get("body").is_none());

    // With approval off, writes run at once, within the key's permissions.
    let s: Value = client
        .put("/api/assistant/settings")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "approval": "off" }))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(s["approval"], "off");
    let (text, err) = call(
        &client,
        &secret,
        "create_product",
        json!({ "body": { "sku": "SOCK", "title": "Sock", "price_minor": 2, "currency": "EUR" } }),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("SOCK"));
    assert_eq!(products(&client, &admin).await["total"], 2);

    // Bad arguments are the tool's error, not the protocol's.
    let (text, err) = call(&client, &secret, "get_product", json!({})).await;
    assert!(err);
    assert_eq!(text, "`id` is required");
    let (text, err) = call(
        &client,
        &secret,
        "get_product",
        json!({ "id": "prod_nope" }),
    )
    .await;
    assert!(err);
    assert!(text.starts_with("HTTP 404"), "{text}");
    let r = rpc(&client, &secret, "tools/call", json!({ "name": "nope" })).await;
    assert_eq!(r["error"]["code"], -32602);
    let r = rpc(&client, &secret, "resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);
}

#[tokio::test]
async fn claude_can_do_only_what_its_key_allows() {
    let (client, admin) = common::with_tenants(&["acme"]).await;

    // Without `assistant:use`, no MCP at all.
    let plain = key(&client, &admin, &["products:read"]).await;
    client
        .post("/mcp")
        .bearer(&plain)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .assert_status(403);
    // Nor anonymously.
    client
        .post("/mcp")
        .header("x-soat-tenant", "acme")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .assert_status(401);

    // A key that may only use the assistant reads nothing through it.
    let bare = key(&client, &admin, &["assistant:use"]).await;
    let (text, err) = call(&client, &bare, "list_products", json!({})).await;
    assert!(err);
    assert!(text.starts_with("HTTP 403"), "{text}");
    let (text, err) = call(&client, &bare, "list_orders", json!({ "limit": 5 })).await;
    assert!(err, "{text}");
    // It may propose, but the approver's own permissions decide, and the key
    // cannot see or decide proposals.
    client
        .get("/api/assistant/proposals")
        .bearer(&bare)
        .send()
        .await
        .assert_status(403);
    client
        .put("/api/assistant/settings")
        .bearer(&bare)
        .json(&json!({ "approval": "off" }))
        .send()
        .await
        .assert_status(403);

    // A staff session works too, naming the shop.
    let r: Value = client
        .post("/mcp")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "jsonrpc": "2.0", "id": "a", "method": "ping" }))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(r, json!({ "jsonrpc": "2.0", "id": "a", "result": {} }));
    client
        .post("/mcp")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .body("{not json")
        .send()
        .await
        .assert_status(400);
    client
        .put("/api/assistant/settings")
        .header("x-soat-tenant", "acme")
        .bearer(&admin)
        .json(&json!({ "approval": "sometimes" }))
        .send()
        .await
        .assert_status(422);
}
