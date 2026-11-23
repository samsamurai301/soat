//! A small client for Stripe's form-encoded REST API.

use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use zapline::prelude::*;

use crate::StripeConfig;

const TIMEOUT: Duration = Duration::from_secs(20);

type HttpsClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

// Zapline follow-up: a public outbound HTTP client, shared with webhooks.
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

/// `application/x-www-form-urlencoded`, as Stripe takes it.
pub(crate) fn form(pairs: &[(&str, String)]) -> String {
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

/// POSTs to Stripe; `account` is a connected account (`acct_...`),
/// `idempotency` makes retries safe. A Stripe error is a 422 with its
/// message (code `stripe_error`); no answer is a 503.
pub(crate) async fn post(
    config: &StripeConfig,
    path: &str,
    pairs: &[(&str, String)],
    account: Option<&str>,
    idempotency: Option<&str>,
) -> Result<Value> {
    let unavailable = |e: String| Error::service_unavailable(format!("Stripe: {e}"));
    let mut req = http::Request::post(format!("{}{path}", config.api_base))
        .header("authorization", format!("Bearer {}", config.secret_key))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("stripe-version", "2024-06-20")
        .header("user-agent", "SOAT/1");
    if let Some(a) = account {
        req = req.header("stripe-account", a);
    }
    if let Some(k) = idempotency {
        req = req.header("idempotency-key", k);
    }
    let req = req
        .body(Full::new(Bytes::from(form(pairs))))
        .map_err(|e| unavailable(e.to_string()))?;
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
    let json: Value = serde_json::from_slice(&body)
        .map_err(|_| unavailable(format!("answered {status} without JSON")))?;
    if status.is_success() {
        Ok(json)
    } else if status.is_server_error() {
        Err(unavailable(format!("answered {status}")))
    } else {
        let message = json["error"]["message"]
            .as_str()
            .unwrap_or("the request was refused")
            .to_owned();
        Err(Error::unprocessable(format!("Stripe: {message}")).with_code("stripe_error"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(
            form(&[("metadata[a b]", "x&y=é".into()), ("n", "1".into())]),
            "metadata%5Ba%20b%5D=x%26y%3D%C3%A9&n=1"
        );
    }
}
