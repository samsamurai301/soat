//! Webhooks end to end against a local receiver.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use soat_auth::Auth;
use soat_catalog::Catalog;
use soat_events::{dispatch_resources, Events};
use soat_module::Modules;
use soat_tenancy::Tenancy;
use soat_webhooks::{send_resources, verify, Webhooks};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zapline::db::Database;
use zapline::prelude::*;
use zapline::test::TestClient;

/// A received request: its headers (lowercased names) and body.
type Received = Arc<Mutex<Vec<(Vec<(String, String)>, Vec<u8>)>>>;

/// A tiny HTTP server answering every request with `status`.
async fn receiver(status: u16) -> (String, Received) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let seen: Received = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            let headers = text[..end]
                                .lines()
                                .skip(1)
                                .filter_map(|l| l.split_once(':'))
                                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                                .collect();
                            log.lock()
                                .unwrap()
                                .push((headers, buf[end + 4..end + 4 + len].to_vec()));
                            break;
                        }
                    }
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (url, seen)
}

async fn app() -> (TestClient, String) {
    let url = soat_testing::fresh_database_url().await;
    let modules = Modules::new()
        .with(Auth::with_setup_token("t"))
        .with(Tenancy)
        .with(Events::manual())
        .with(Webhooks::new().manual().allow_private_targets())
        .with(Catalog);
    let client = modules.build(App::new(), Database::postgres(url)).test();
    let session: Value = client
        .post("/api/setup")
        .json(&json!({ "setup_token": "t", "email": "a@example.com", "password": "admin password 1" }))
        .send()
        .await
        .assert_status(201)
        .json();
    let token = session["token"].as_str().unwrap().to_owned();
    client
        .post("/api/tenants")
        .bearer(&token)
        .json(&json!({ "id": "acme", "name": "Acme" }))
        .send()
        .await
        .assert_status(201);
    (client, token)
}

async fn register(client: &TestClient, token: &str, url: &str, events: Value) -> Value {
    client
        .post("/api/webhooks")
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "url": url, "events": events }))
        .send()
        .await
        .assert_status(201)
        .json()
}

async fn create_product(client: &TestClient, token: &str) {
    client
        .post("/api/products")
        .header("x-soat-tenant", "acme")
        .bearer(token)
        .json(&json!({ "sku": "S", "title": "Shoe", "price_minor": 1, "currency": "EUR" }))
        .send()
        .await
        .assert_status(201);
}

async fn run(client: &TestClient) -> usize {
    let res = client.resources().await;
    dispatch_resources(&res).await.unwrap();
    send_resources(&res).await.unwrap()
}

#[tokio::test]
async fn events_are_posted_signed_to_matching_endpoints() {
    let (client, token) = app().await;
    let (url, seen) = receiver(200).await;
    let created = register(&client, &token, &url, json!(["product.created"])).await;
    let (other, other_seen) = receiver(200).await;
    register(&client, &token, &other, json!(["order.placed"])).await;

    create_product(&client, &token).await;
    assert_eq!(run(&client).await, 1);
    assert_eq!(run(&client).await, 0);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(other_seen.lock().unwrap().is_empty());
    let (headers, body) = &seen[0];
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let secret = created["secret"].as_str().unwrap();
    assert!(verify(secret, &header("soat-signature").unwrap(), body));
    let body: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(body["type"], "product.created");
    assert_eq!(body["data"]["sku"], "S");
}

#[tokio::test]
async fn failing_receivers_are_retried_and_logged() {
    let (client, token) = app().await;
    let (url, seen) = receiver(500).await;
    let created = register(&client, &token, &url, json!(["*"])).await;
    create_product(&client, &token).await;
    assert_eq!(run(&client).await, 0);
    assert_eq!(seen.lock().unwrap().len(), 1);

    let id = created["endpoint"]["id"].as_str().unwrap();
    let log: Value = client
        .get(&format!("/api/webhooks/{id}/deliveries"))
        .header("x-soat-tenant", "acme")
        .bearer(&token)
        .send()
        .await
        .assert_status(200)
        .json();
    let delivery = &log["items"][0];
    assert_eq!(delivery["attempts"], 1);
    assert_eq!(delivery["delivered"], false);
    assert_eq!(delivery["last_status"], 500);
    // Backoff: not retried right away.
    assert_eq!(run(&client).await, 0);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn private_targets_are_refused_in_production() {
    let url = soat_testing::fresh_database_url().await;
    let modules = Modules::new()
        .with(Auth::with_setup_token("t"))
        .with(Tenancy)
        .with(Events::manual())
        .with(Webhooks::new().manual());
    let client = modules.build(App::new(), Database::postgres(url)).test();
    let session: Value = client
        .post("/api/setup")
        .json(&json!({ "setup_token": "t", "email": "a@example.com", "password": "admin password 1" }))
        .send()
        .await
        .json();
    let token = session["token"].as_str().unwrap();
    client
        .post("/api/tenants")
        .bearer(token)
        .json(&json!({ "id": "acme", "name": "Acme" }))
        .send()
        .await
        .assert_status(201);
    for url in [
        "http://example.com/x",
        "https://169.254.169.254/latest",
        "https://localhost/x",
    ] {
        client
            .post("/api/webhooks")
            .header("x-soat-tenant", "acme")
            .bearer(token)
            .json(&json!({ "url": url, "events": ["*"] }))
            .send()
            .await
            .assert_status(422);
    }
}
