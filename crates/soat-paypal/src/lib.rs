//! PayPal, as a payment provider (Orders v2).
//!
//! The platform holds one PayPal REST app (`SOAT_PAYPAL_CLIENT_ID`,
//! `SOAT_PAYPAL_CLIENT_SECRET`); a shop's payment method may name the
//! merchant account paid (`merchant_id`, for platforms onboarding
//! sellers), so no shop secret is stored. Starting a payment creates a
//! PayPal order and sends the buyer to approve it. PayPal's webhooks
//! (`POST /api/payments/paypal/webhook`, verified with PayPal against
//! `SOAT_PAYPAL_WEBHOOK_ID`) say when it was approved; the server then
//! captures it (or authorizes it, to capture later) and reads the order
//! back from PayPal, never trusting the event's body.
//!
//! Method settings: `merchant_id` (optional) and `intent` (`capture`, the
//! default, or `authorize`).

mod api;
mod webhook;

use std::sync::Arc;

use serde_json::{json, Value};
use soat_core::Currency;
use soat_module::Module;
use soat_payments::http::Method;
use soat_payments::{
    PaymentProvider, PaymentStatus, ProviderFuture, ProviderPayment, ProviderUpdate, StartRequest,
};
use zapline::prelude::*;

/// How this server reaches PayPal.
#[derive(Clone)]
pub struct PayPalConfig {
    pub client_id: String,
    pub client_secret: String,
    /// The id of the webhook registered for this server; events are
    /// verified against it.
    pub webhook_id: String,
    /// `https://api-m.paypal.com`, or `https://api-m.sandbox.paypal.com`.
    pub api_base: String,
}

impl PayPalConfig {
    /// From `SOAT_PAYPAL_CLIENT_ID`, `SOAT_PAYPAL_CLIENT_SECRET`,
    /// `SOAT_PAYPAL_WEBHOOK_ID` and optionally `SOAT_PAYPAL_API_BASE`;
    /// `None` without a client id.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            client_id: std::env::var("SOAT_PAYPAL_CLIENT_ID").ok()?,
            client_secret: std::env::var("SOAT_PAYPAL_CLIENT_SECRET").unwrap_or_default(),
            webhook_id: std::env::var("SOAT_PAYPAL_WEBHOOK_ID").unwrap_or_default(),
            api_base: std::env::var("SOAT_PAYPAL_API_BASE")
                .unwrap_or_else(|_| "https://api-m.paypal.com".to_owned()),
        })
    }
}

/// What the provider and its webhooks share.
pub(crate) struct Inner {
    config: PayPalConfig,
    token: api::Token,
}

impl Inner {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        request_id: Option<&str>,
        merchant: Option<&str>,
    ) -> Result<Value> {
        api::call(
            &self.config,
            &self.token,
            method,
            path,
            body,
            request_id,
            merchant,
        )
        .await
    }

    async fn order(&self, id: &str) -> Result<Value> {
        self.call(
            Method::GET,
            &format!("/v2/checkout/orders/{id}"),
            None,
            None,
            None,
        )
        .await
    }

    /// Captures or authorizes an approved order, as it asked; returns the
    /// order as PayPal then has it.
    async fn complete(&self, order: &Value, request_id: &str) -> Result<Value> {
        let id = order["id"].as_str().unwrap_or_default();
        let step = if order["intent"] == "AUTHORIZE" {
            "authorize"
        } else {
            "capture"
        };
        self.call(
            Method::POST,
            &format!("/v2/checkout/orders/{id}/{step}"),
            Some(&json!({})),
            Some(&format!("{request_id}-{step}")),
            None,
        )
        .await
    }
}

/// The PayPal payment provider; register it with
/// `Payments::new().with(PayPal::new(config.clone()))` and add
/// [`PayPalWebhooks`] (sharing it) for its events.
#[derive(Clone)]
pub struct PayPal {
    inner: Arc<Inner>,
}

impl PayPal {
    pub fn new(config: PayPalConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                token: api::Token::default(),
            }),
        }
    }

    /// The module receiving PayPal's webhooks for this provider.
    pub fn webhooks(&self) -> PayPalWebhooks {
        PayPalWebhooks {
            inner: self.inner.clone(),
        }
    }
}

/// The first purchase unit's payments of an order.
fn payments_of(order: &Value) -> &Value {
    &order["purchase_units"][0]["payments"]
}

/// What a PayPal order means for the payment.
pub fn order_update(order: &Value) -> ProviderUpdate {
    let capture = &payments_of(order)["captures"][0];
    let authorization = &payments_of(order)["authorizations"][0];
    let (status, failure) = match order["status"].as_str().unwrap_or_default() {
        "APPROVED" => (PaymentStatus::Pending, None),
        "VOIDED" => (PaymentStatus::Canceled, None),
        "COMPLETED" => match (capture["status"].as_str(), authorization["status"].as_str()) {
            (Some("COMPLETED"), _) => (PaymentStatus::Paid, None),
            (Some("PENDING"), _) => (PaymentStatus::Pending, None),
            (Some("DECLINED" | "FAILED"), _) => (
                PaymentStatus::Failed,
                Some("PayPal declined the payment".to_owned()),
            ),
            (_, Some("CREATED" | "PENDING")) => (PaymentStatus::Authorized, None),
            (_, Some("CAPTURED")) => (PaymentStatus::Paid, None),
            (_, Some("VOIDED" | "EXPIRED")) => (PaymentStatus::Canceled, None),
            _ => (PaymentStatus::Pending, None),
        },
        _ => (PaymentStatus::RequiresAction, None),
    };
    let approve = order["links"].as_array().and_then(|links| {
        links
            .iter()
            .find(|l| l["rel"] == "payer-action" || l["rel"] == "approve")
            .and_then(|l| l["href"].as_str())
    });
    let next_action = approve
        .filter(|_| status == PaymentStatus::RequiresAction)
        .map(|url| json!({ "type": "redirect", "url": url }));
    ProviderUpdate {
        provider_ref: order["id"].as_str().map(str::to_owned),
        next_action,
        failure_reason: failure,
        ..ProviderUpdate::status(status)
    }
}

fn amount(currency: &str, minor: i64) -> Result<Value> {
    let c = Currency::parse(currency).map_err(|e| Error::unprocessable(e.to_string()))?;
    Ok(json!({ "currency_code": c.as_str(), "value": c.decimal(minor) }))
}

fn merchant(settings: &Value) -> Option<&str> {
    settings.get("merchant_id").and_then(Value::as_str)
}

fn no_order() -> Error {
    Error::conflict("PayPal has no order for this payment yet").with_code("not_started")
}

impl PaymentProvider for PayPal {
    fn name(&self) -> &'static str {
        "paypal"
    }

    fn label(&self) -> &'static str {
        "PayPal: PayPal balance, Pay Later and cards through PayPal"
    }

    fn check_settings(&self, settings: &Value) -> Result<()> {
        let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_settings"));
        let Some(o) = settings.as_object() else {
            return bad("settings are an object");
        };
        for (k, v) in o {
            let s = v.as_str().unwrap_or_default();
            let ok = match k.as_str() {
                "merchant_id" => {
                    (10..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
                }
                "intent" => matches!(s, "capture" | "authorize"),
                _ => false,
            };
            if !ok {
                return bad(&format!(
                    "{k} is not a PayPal setting here: use merchant_id or intent (capture or \
                     authorize); credentials belong in the server's environment"
                ));
            }
        }
        Ok(())
    }

    fn start<'a>(&'a self, req: &'a StartRequest<'a>) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let back = req.return_url.ok_or_else(|| {
                Error::unprocessable("PayPal sends the buyer back: give a return_url")
                    .with_code("return_url_required")
            })?;
            let intent = if req.settings["intent"] == "authorize" {
                "AUTHORIZE"
            } else {
                "CAPTURE"
            };
            let mut unit = json!({
                "reference_id": req.reference,
                "custom_id": format!("{}:{}", req.tenant_id, req.payment_id),
                "amount": amount(req.currency, req.amount_minor)?,
            });
            if let Some(m) = merchant(req.settings) {
                unit["payee"] = json!({ "merchant_id": m });
            }
            let body = json!({
                "intent": intent,
                "purchase_units": [unit],
                "payment_source": { "paypal": { "experience_context": {
                    "return_url": back,
                    "cancel_url": back,
                    "user_action": "PAY_NOW",
                } } },
            });
            let order = self
                .inner
                .call(
                    Method::POST,
                    "/v2/checkout/orders",
                    Some(&body),
                    Some(req.payment_id),
                    None,
                )
                .await?;
            Ok(order_update(&order))
        })
    }

    fn capture<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
    ) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let id = payment.provider_ref.ok_or_else(no_order)?;
            let order = self.inner.order(id).await?;
            let auth = payments_of(&order)["authorizations"][0]["id"]
                .as_str()
                .ok_or_else(|| {
                    Error::conflict("the buyer has not approved this PayPal payment yet")
                        .with_code("not_authorized")
                })?;
            self.inner
                .call(
                    Method::POST,
                    &format!("/v2/payments/authorizations/{auth}/capture"),
                    Some(&json!({ "final_capture": true })),
                    Some(&format!("{}-capture", payment.payment_id)),
                    merchant(payment.settings),
                )
                .await?;
            Ok(order_update(&self.inner.order(id).await?))
        })
    }

    fn cancel<'a>(&'a self, payment: &'a ProviderPayment<'a>) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let Some(id) = payment.provider_ref else {
                return Ok(());
            };
            let order = self.inner.order(id).await?;
            // An order never approved simply expires at PayPal.
            if let Some(auth) = payments_of(&order)["authorizations"][0]["id"].as_str() {
                self.inner
                    .call(
                        Method::POST,
                        &format!("/v2/payments/authorizations/{auth}/void"),
                        None,
                        Some(&format!("{}-void", payment.payment_id)),
                        merchant(payment.settings),
                    )
                    .await?;
            }
            Ok(())
        })
    }

    fn refund<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
        amount_minor: i64,
    ) -> ProviderFuture<'a, Option<String>> {
        Box::pin(async move {
            let id = payment.provider_ref.ok_or_else(no_order)?;
            let order = self.inner.order(id).await?;
            let capture = payments_of(&order)["captures"][0]["id"]
                .as_str()
                .ok_or_else(|| {
                    Error::conflict("PayPal has not captured this payment")
                        .with_code("not_captured")
                })?;
            let r = self
                .inner
                .call(
                    Method::POST,
                    &format!("/v2/payments/captures/{capture}/refund"),
                    Some(&json!({ "amount": amount(payment.currency, amount_minor)? })),
                    None,
                    merchant(payment.settings),
                )
                .await?;
            Ok(r["id"].as_str().map(str::to_owned))
        })
    }
}

/// Receives PayPal's webhook events.
pub struct PayPalWebhooks {
    inner: Arc<Inner>,
}

impl Module for PayPalWebhooks {
    fn name(&self) -> &'static str {
        "paypal"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "payments"]
    }

    fn mount(&self, app: App) -> App {
        app.state(webhook::Shared(self.inner.clone()))
            .plugin(webhook::mount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        let o = json!({ "id": "O1", "status": "PAYER_ACTION_REQUIRED",
            "links": [{ "rel": "self", "href": "x" }, { "rel": "payer-action", "href": "https://paypal.test/approve" }] });
        let u = order_update(&o);
        assert_eq!(u.status, PaymentStatus::RequiresAction);
        assert_eq!(u.next_action.unwrap()["url"], "https://paypal.test/approve");
        let paid = json!({ "id": "O1", "status": "COMPLETED",
            "purchase_units": [{ "payments": { "captures": [{ "id": "C1", "status": "COMPLETED" }] } }] });
        assert_eq!(order_update(&paid).status, PaymentStatus::Paid);
        let held = json!({ "id": "O1", "status": "COMPLETED",
            "purchase_units": [{ "payments": { "authorizations": [{ "id": "A1", "status": "CREATED" }] } }] });
        assert_eq!(order_update(&held).status, PaymentStatus::Authorized);
        let declined = json!({ "id": "O1", "status": "COMPLETED",
            "purchase_units": [{ "payments": { "captures": [{ "id": "C1", "status": "DECLINED" }] } }] });
        assert_eq!(order_update(&declined).status, PaymentStatus::Failed);
        assert_eq!(
            order_update(&json!({ "status": "APPROVED" })).status,
            PaymentStatus::Pending
        );
    }
}
