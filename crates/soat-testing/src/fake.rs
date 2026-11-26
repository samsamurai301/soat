//! A fake HTTP API on a local port, for testing integrations (payment
//! providers, carriers) without the network.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct Received {
    pub method: String,
    /// With the query string.
    pub path: String,
    /// Lowercased names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Received {
    /// The first header called `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The body as JSON; `Null` when it is not.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// Every request the fake received, in order.
pub type Log = Arc<Mutex<Vec<Received>>>;

/// Serves `answer` (status and JSON body, or `{"raw": text,
/// "content_type": type}` for other bytes) for every request; returns the
/// base URL (`http://127.0.0.1:port`) and the request log.
pub async fn fake_api<F>(answer: F) -> (String, Log)
where
    F: Fn(&Received) -> (u16, Value) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let log: Log = Arc::default();
    let seen = log.clone();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            let answer = answer.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let request = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let head = &text[..end];
                    let headers: Vec<(String, String)> = head
                        .lines()
                        .skip(1)
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                        .collect();
                    let len = headers
                        .iter()
                        .find(|(k, _)| k == "content-length")
                        .map_or(0, |(_, v)| v.parse::<usize>().unwrap_or(0));
                    if buf.len() >= end + 4 + len {
                        let mut first = head.split(' ');
                        break Received {
                            method: first.next().unwrap_or_default().to_owned(),
                            path: first.next().unwrap_or_default().to_owned(),
                            headers,
                            body: String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string(),
                        };
                    }
                };
                let (status, reply) = answer(&request);
                seen.lock().unwrap_or_else(|e| e.into_inner()).push(request);
                // `{"raw": "...", "content_type": "..."}` answers bytes.
                let (kind, reply) = match reply["raw"].as_str() {
                    Some(raw) => (
                        reply["content_type"]
                            .as_str()
                            .unwrap_or("application/octet-stream")
                            .to_owned(),
                        raw.to_owned(),
                    ),
                    None if reply.is_null() => ("application/json".to_owned(), String::new()),
                    None => ("application/json".to_owned(), reply.to_string()),
                };
                let out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(out.as_bytes()).await;
            });
        }
    });
    (base, log)
}
