//! Mollie, as a payment provider: iDEAL, Bancontact, cards, Klarna, EPS,
//! Przelewy24 and the other methods Mollie offers across Europe.
//!
//! The platform holds one Mollie key (`SOAT_MOLLIE_API_KEY`): an API key
//! for a single shop, or an organization access token (Mollie Connect)
//! with each shop's payment method naming its own website profile
//! (`profile_id`), so no shop secret is ever stored. Starting a payment
//! creates a Mollie payment and sends the buyer to its checkout page.
//! Mollie calls `POST /api/payments/mollie/webhook` with only the
//! payment's id; the server then asks Mollie for the payment, so a caller
//! cannot make anything up.
//!
//! Method settings: `profile_id` (`pfl_...`, with an organization token)
//! and `method` (e.g. `ideal`; omitted, the buyer picks on Mollie's page).

mod webhook;

use std::sync::Arc;

use serde_json::{json, Value};
use soat_core::Currency;
use soat_module::Module;
use soat_payments::http::{send, Bytes, Full, Method, Request};
use soat_payments::{
    PaymentProvider, PaymentStatus, ProviderFuture, ProviderPayment, ProviderUpdate, StartRequest,
};
use zapline::prelude::*;

/// The webhook path Mollie calls.
pub const WEBHOOK_PATH: &str = "/api/payments/mollie/webhook";

/// How this server reaches Mollie.
#[derive(Clone)]
pub struct MollieConfig {
    /// `live_...`, `test_...` or an organization access token `access_...`.
    pub api_key: String,
    /// `https://api.mollie.com`; tests point it at a local fake.
    pub api_base: String,
    /// Where Mollie reaches this server, e.g. `https://shop.example`; empty
    /// for no webhooks (local development).
    pub public_url: String,
    /// Test mode, for organization access tokens.
    pub testmode: bool,
}

impl MollieConfig {
    /// From `SOAT_MOLLIE_API_KEY`, `SOAT_PUBLIC_URL`, and optionally
    /// `SOAT_MOLLIE_API_BASE` and `SOAT_MOLLIE_TESTMODE=true`; `None`
    /// without a key.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("SOAT_MOLLIE_API_KEY").ok()?;
        Some(Self {
            api_key,
            api_base: std::env::var("SOAT_MOLLIE_API_BASE")
                .unwrap_or_else(|_| "https://api.mollie.com".to_owned()),
            public_url: std::env::var("SOAT_PUBLIC_URL").unwrap_or_default(),
            testmode: std::env::var("SOAT_MOLLIE_TESTMODE").is_ok_and(|v| v == "true"),
        })
    }

    fn organization(&self) -> bool {
        self.api_key.starts_with("access_")
    }
}

/// Calls Mollie. A Mollie error is a 422 with its detail (code
/// `mollie_error`); no answer is a 503.
pub(crate) async fn call(
    config: &MollieConfig,
    method: Method,
    path: &str,
    body: Option<&Value>,
    idempotency: Option<&str>,
) -> Result<Value> {
    let unavailable = |e: String| Error::service_unavailable(format!("Mollie: {e}"));
    let mut url = format!("{}{path}", config.api_base);
    if config.organization() && config.testmode && body.is_none() {
        url.push_str(if url.contains('?') { "&" } else { "?" });
        url.push_str("testmode=true");
    }
    let mut req = Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", format!("Bearer {}", config.api_key))
        .header("accept", "application/json")
        .header("user-agent", "SOAT/1");
    if let Some(k) = idempotency {
        req = req.header("idempotency-key", k);
    }
    let payload = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Bytes::from(b.to_string())
        }
        None => Bytes::new(),
    };
    let req = req
        .body(Full::new(payload))
        .map_err(|e| unavailable(e.to_string()))?;
    let answer = send("Mollie", req).await?;
    let status = answer.status;
    let json = answer.json();
    if status.is_success() {
        Ok(json)
    } else if status.is_server_error() {
        Err(unavailable(format!("answered {status}")))
    } else {
        let detail = json["detail"]
            .as_str()
            .unwrap_or("the request was refused")
            .to_owned();
        Err(Error::unprocessable(format!("Mollie: {detail}")).with_code("mollie_error"))
    }
}

/// The Mollie payment provider; register it with
/// `Payments::new().with(Mollie::new(config.clone()))` and add
/// [`MollieWebhooks`] for its calls.
pub struct Mollie {
    config: Arc<MollieConfig>,
}

impl Mollie {
    pub fn new(config: MollieConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

/// What a Mollie payment's status means for the payment.
pub fn payment_update(p: &Value) -> ProviderUpdate {
    let status = match p["status"].as_str().unwrap_or_default() {
        "paid" => PaymentStatus::Paid,
        "authorized" => PaymentStatus::Authorized,
        "pending" => PaymentStatus::Pending,
        "canceled" => PaymentStatus::Canceled,
        "expired" | "failed" => PaymentStatus::Failed,
        _ => PaymentStatus::RequiresAction,
    };
    let failure_reason = match p["status"].as_str() {
        Some("expired") => Some("the buyer did not finish paying in time".to_owned()),
        Some("failed") => p["details"]["failureMessage"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| Some("the payment failed".to_owned())),
        _ => None,
    };
    let next_action = p["_links"]["checkout"]["href"]
        .as_str()
        .filter(|_| status == PaymentStatus::RequiresAction)
        .map(|url| json!({ "type": "redirect", "url": url }));
    ProviderUpdate {
        provider_ref: p["id"].as_str().map(str::to_owned),
        next_action,
        failure_reason,
        ..ProviderUpdate::status(status)
    }
}

fn amount(currency: &str, minor: i64) -> Result<Value> {
    let c = Currency::parse(currency).map_err(|e| Error::unprocessable(e.to_string()))?;
    Ok(json!({ "currency": c.as_str(), "value": c.decimal(minor) }))
}

fn no_payment() -> Error {
    Error::conflict("Mollie has no payment for this payment yet").with_code("not_started")
}

impl PaymentProvider for Mollie {
    fn name(&self) -> &'static str {
        "mollie"
    }

    fn label(&self) -> &'static str {
        "Mollie: iDEAL, Bancontact, cards, Klarna, EPS, Przelewy24 and more"
    }

    fn check_settings(&self, settings: &Value) -> Result<()> {
        let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_settings"));
        let Some(o) = settings.as_object() else {
            return bad("settings are an object");
        };
        for (k, v) in o {
            let s = v.as_str().unwrap_or_default();
            let ok = match k.as_str() {
                "profile_id" => {
                    s.starts_with("pfl_")
                        && s.len() <= 64
                        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }
                "method" => {
                    (2..=32).contains(&s.len())
                        && s.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                }
                _ => false,
            };
            if !ok {
                return bad(&format!(
                    "{k} is not a Mollie setting here: use profile_id (pfl_) or method \
                     (e.g. ideal); API keys belong in the server's environment"
                ));
            }
        }
        if self.config.organization() && settings.get("profile_id").is_none() {
            return bad("give the shop's Mollie profile_id (pfl_...)");
        }
        Ok(())
    }

    fn start<'a>(&'a self, req: &'a StartRequest<'a>) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let redirect = req.return_url.ok_or_else(|| {
                Error::unprocessable("Mollie sends the buyer back: give a return_url")
                    .with_code("return_url_required")
            })?;
            let mut body = json!({
                "amount": amount(req.currency, req.amount_minor)?,
                "description": format!("Payment {}", req.reference),
                "redirectUrl": redirect,
                "metadata": {
                    "soat_tenant": req.tenant_id,
                    "soat_payment": req.payment_id,
                    "soat_reference": req.reference,
                },
            });
            if !self.config.public_url.is_empty() {
                body["webhookUrl"] = json!(format!(
                    "{}{WEBHOOK_PATH}",
                    self.config.public_url.trim_end_matches('/')
                ));
            }
            if let Some(m) = req.settings.get("method") {
                body["method"] = m.clone();
            }
            if let Some(p) = req.settings.get("profile_id") {
                body["profileId"] = p.clone();
            }
            if self.config.organization() && self.config.testmode {
                body["testmode"] = json!(true);
            }
            let p = call(
                &self.config,
                Method::POST,
                "/v2/payments",
                Some(&body),
                Some(req.payment_id),
            )
            .await?;
            Ok(payment_update(&p))
        })
    }

    fn capture<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
    ) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let id = payment.provider_ref.ok_or_else(no_payment)?;
            let mut body = json!({});
            if self.config.organization() && self.config.testmode {
                body["testmode"] = json!(true);
            }
            call(
                &self.config,
                Method::POST,
                &format!("/v2/payments/{id}/captures"),
                Some(&body),
                Some(&format!("{}-capture", payment.payment_id)),
            )
            .await?;
            let p = call(
                &self.config,
                Method::GET,
                &format!("/v2/payments/{id}"),
                None,
                None,
            )
            .await?;
            Ok(payment_update(&p))
        })
    }

    fn cancel<'a>(&'a self, payment: &'a ProviderPayment<'a>) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let Some(id) = payment.provider_ref else {
                return Ok(());
            };
            call(
                &self.config,
                Method::DELETE,
                &format!("/v2/payments/{id}"),
                None,
                None,
            )
            .await?;
            Ok(())
        })
    }

    fn refund<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
        amount_minor: i64,
    ) -> ProviderFuture<'a, Option<String>> {
        Box::pin(async move {
            let id = payment.provider_ref.ok_or_else(no_payment)?;
            let mut body = json!({
                "amount": amount(payment.currency, amount_minor)?,
                "metadata": { "soat_payment": payment.payment_id },
            });
            if self.config.organization() && self.config.testmode {
                body["testmode"] = json!(true);
            }
            let r = call(
                &self.config,
                Method::POST,
                &format!("/v2/payments/{id}/refunds"),
                Some(&body),
                None,
            )
            .await?;
            Ok(r["id"].as_str().map(str::to_owned))
        })
    }
}

/// Receives Mollie's webhook calls.
pub struct MollieWebhooks {
    config: Arc<MollieConfig>,
}

impl MollieWebhooks {
    pub fn new(config: MollieConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl Module for MollieWebhooks {
    fn name(&self) -> &'static str {
        "mollie"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "payments"]
    }

    fn mount(&self, app: App) -> App {
        app.state(webhook::Config(self.config.clone()))
            .plugin(webhook::mount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        let p = json!({ "id": "tr_1", "status": "open",
            "_links": { "checkout": { "href": "https://www.mollie.com/checkout/x" } } });
        let u = payment_update(&p);
        assert_eq!(u.status, PaymentStatus::RequiresAction);
        assert_eq!(
            u.next_action.unwrap()["url"],
            "https://www.mollie.com/checkout/x"
        );
        assert_eq!(u.provider_ref.as_deref(), Some("tr_1"));
        let u = payment_update(&json!({ "id": "tr_1", "status": "paid" }));
        assert_eq!(u.status, PaymentStatus::Paid);
        assert!(u.next_action.is_none());
        let u = payment_update(&json!({ "id": "tr_1", "status": "expired" }));
        assert_eq!(u.status, PaymentStatus::Failed);
        assert!(u.failure_reason.is_some());
        assert_eq!(
            amount("EUR", 1999).unwrap(),
            json!({ "currency": "EUR", "value": "19.99" })
        );
    }
}
