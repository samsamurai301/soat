//! A small client for Stripe's form-encoded REST API.

use serde_json::Value;
use soat_payments::http::{form, send, Bytes, Full, Request};
use zapline::prelude::*;

use crate::StripeConfig;

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
    let mut req = Request::post(format!("{}{path}", config.api_base))
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
    let answer = send("Stripe", req).await?;
    let status = answer.status;
    let json = answer.json();
    if json.is_null() {
        return Err(unavailable(format!("answered {status} without JSON")));
    }
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
