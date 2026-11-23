//! A small client for Anthropic's Messages API, over HTTPS with rustls and
//! the Mozilla roots (plain HTTP for local test doubles). There is no
//! official Rust SDK, so this sends the documented JSON as it is.
//!
//! Zapline follow-up: the same outbound client lives in `soat-payments`;
//! once Zapline offers one, both go.

use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use http::Request;
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use zapline::prelude::*;

use crate::keys::AnthropicKey;

/// The API version every request names.
const API_VERSION: &str = "2026-10-21";

/// Claude may think and write for a while; this bounds one request.
const TIMEOUT: Duration = Duration::from_secs(600);

type HttpsClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

fn client() -> &'static HttpsClient {
    static CLIENT: OnceLock<HttpsClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder(TokioExecutor::new()).build(https)
    })
}

/// Sends `body` to `POST {base}/v1/messages` with the shop's key; answers
/// the message. A key Anthropic refuses is a 422 `assistant_key_rejected`,
/// a request it refuses a 422 `anthropic_error` with its message, and an
/// overloaded or unreachable API a 503.
pub(crate) async fn messages(base: &str, key: &AnthropicKey, body: &Value) -> Result<Value> {
    let unavailable = |e: String| Error::service_unavailable(format!("Claude: {e}"));
    let req = Request::post(format!("{}/v1/messages", base.trim_end_matches('/')))
        .header("x-api-key", &key.api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .header("user-agent", "SOAT/1")
        .body(Full::new(Bytes::from(body.to_string())))
        .map_err(|e| unavailable(e.to_string()))?;
    let res = tokio::time::timeout(TIMEOUT, client().request(req))
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?;
    let status = res.status();
    let bytes = tokio::time::timeout(TIMEOUT, res.into_body().collect())
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?
        .to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        return if json.is_object() {
            Ok(json)
        } else {
            Err(unavailable("answered without JSON".into()))
        };
    }
    let message = json["error"]["message"]
        .as_str()
        .unwrap_or("the request was refused")
        .to_owned();
    match status.as_u16() {
        401 | 403 => Err(Error::unprocessable(format!(
            "Anthropic did not accept the shop's key: {message}"
        ))
        .with_code("assistant_key_rejected")),
        429 | 500.. => Err(unavailable(format!("busy ({status}): {message}"))),
        _ => Err(Error::unprocessable(format!("Claude: {message}")).with_code("anthropic_error")),
    }
}
