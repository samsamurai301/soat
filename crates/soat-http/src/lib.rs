//! An outbound HTTPS client for adapters to outside services (payment
//! providers, carriers): hyper with rustls and the Mozilla roots, plain
//! HTTP allowed for local test doubles, a timeout on every call.
//!
//! Zapline follow-up: Zapline has the same client inside (S3, OAuth);
//! once it is public there, this goes.

use std::sync::OnceLock;
use std::time::Duration;

pub use bytes::Bytes;
pub use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
pub use http_body_util::Full;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use zapline::prelude::*;

const TIMEOUT: Duration = Duration::from_secs(20);

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

/// A buffered answer.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Answer {
    /// The body as JSON; `Null` when it is empty or not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Sends `req`. No answer in time, or no connection, is a 503 naming
/// `service` (e.g. `Mollie`).
///
/// # Errors
///
/// `503` when the service cannot be reached.
pub async fn send(service: &str, req: Request<Full<Bytes>>) -> Result<Answer> {
    let unavailable = |e: String| Error::service_unavailable(format!("{service}: {e}"));
    let res = tokio::time::timeout(TIMEOUT, client().request(req))
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?;
    let status = res.status();
    let body = tokio::time::timeout(TIMEOUT, res.into_body().collect())
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?
        .to_bytes();
    Ok(Answer { status, body })
}

/// `application/x-www-form-urlencoded` from pairs.
pub fn form(pairs: &[(&str, String)]) -> String {
    fn enc(s: &str, out: &mut String) {
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        enc(k, &mut out);
        out.push('=');
        enc(v, &mut out);
    }
    out
}

/// Reads one field of a form-encoded body, e.g. Mollie's `id=tr_...`.
pub fn form_field(body: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    text.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (decode(k)? == name).then(|| decode(v)).flatten()
    })
}

fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let body = form(&[("metadata[a b]", "x&y=é".into()), ("n", "1".into())]);
        assert_eq!(body, "metadata%5Ba%20b%5D=x%26y%3D%C3%A9&n=1");
        assert_eq!(
            form_field(body.as_bytes(), "metadata[a b]").as_deref(),
            Some("x&y=é")
        );
        assert_eq!(
            form_field(b"id=tr_12+3&x=1", "id").as_deref(),
            Some("tr_12 3")
        );
        assert_eq!(form_field(b"id=%zz", "id"), None);
        assert_eq!(form_field(b"x=1", "id"), None);
    }
}
