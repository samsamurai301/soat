//! A small client for PayPal's REST API: an OAuth token cached until it
//! nearly expires, JSON calls, and the partner's auth assertion.

use std::time::{Duration, Instant};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use soat_payments::http::{form, send, Bytes, Full, Method, Request};
use tokio::sync::Mutex;
use zapline::prelude::*;

use crate::PayPalConfig;

/// The token in use, and when to stop using it.
#[derive(Default)]
pub(crate) struct Token(Mutex<Option<(String, Instant)>>);

fn unavailable(e: String) -> Error {
    Error::service_unavailable(format!("PayPal: {e}"))
}

fn refused(json: &Value) -> Error {
    let detail = json["details"][0]["description"]
        .as_str()
        .or_else(|| json["message"].as_str())
        .or_else(|| json["error_description"].as_str())
        .unwrap_or("the request was refused");
    Error::unprocessable(format!("PayPal: {detail}")).with_code("paypal_error")
}

async fn token(config: &PayPalConfig, cache: &Token) -> Result<String> {
    let mut held = cache.0.lock().await;
    if let Some((t, until)) = held.as_ref() {
        if Instant::now() < *until {
            return Ok(t.clone());
        }
    }
    let basic = STANDARD.encode(format!("{}:{}", config.client_id, config.client_secret));
    let req = Request::post(format!("{}/v1/oauth2/token", config.api_base))
        .header("authorization", format!("Basic {basic}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(Full::new(Bytes::from(form(&[(
            "grant_type",
            "client_credentials".to_owned(),
        )]))))
        .map_err(|e| unavailable(e.to_string()))?;
    let answer = send("PayPal", req).await?;
    let json = answer.json();
    if answer.status.is_server_error() {
        return Err(unavailable(format!("answered {}", answer.status)));
    }
    let Some(t) = json["access_token"]
        .as_str()
        .filter(|_| answer.status.is_success())
    else {
        return Err(Error::service_unavailable(
            "PayPal refused the platform's credentials",
        ));
    };
    // Renew a minute early.
    let life = json["expires_in"]
        .as_u64()
        .unwrap_or(300)
        .saturating_sub(60);
    *held = Some((t.to_owned(), Instant::now() + Duration::from_secs(life)));
    Ok(t.to_owned())
}

/// The header that lets the platform act for a merchant (refunds, voids).
pub(crate) fn auth_assertion(client_id: &str, merchant_id: &str) -> String {
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "none" }).to_string());
    let body =
        URL_SAFE_NO_PAD.encode(json!({ "iss": client_id, "payer_id": merchant_id }).to_string());
    format!("{head}.{body}.")
}

/// Calls PayPal. A PayPal error is a 422 with its description (code
/// `paypal_error`); no answer is a 503.
pub(crate) async fn call(
    config: &PayPalConfig,
    cache: &Token,
    method: Method,
    path: &str,
    body: Option<&Value>,
    request_id: Option<&str>,
    merchant: Option<&str>,
) -> Result<Value> {
    let bearer = token(config, cache).await?;
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{}{path}", config.api_base))
        .header("authorization", format!("Bearer {bearer}"))
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("prefer", "return=representation")
        .header("user-agent", "SOAT/1");
    if let Some(id) = request_id {
        req = req.header("paypal-request-id", id);
    }
    if let Some(m) = merchant {
        req = req.header(
            "paypal-auth-assertion",
            auth_assertion(&config.client_id, m),
        );
    }
    let payload = body.map_or_else(Bytes::new, |b| Bytes::from(b.to_string()));
    let req = req
        .body(Full::new(payload))
        .map_err(|e| unavailable(e.to_string()))?;
    let answer = send("PayPal", req).await?;
    let json = answer.json();
    if answer.status.is_success() {
        Ok(json)
    } else if answer.status.is_server_error() {
        Err(unavailable(format!("answered {}", answer.status)))
    } else {
        Err(refused(&json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertions() {
        assert_eq!(
            auth_assertion("cid", "MERCHANT1"),
            "eyJhbGciOiJub25lIn0.eyJpc3MiOiJjaWQiLCJwYXllcl9pZCI6Ik1FUkNIQU5UMSJ9."
        );
    }
}
