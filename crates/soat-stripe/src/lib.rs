//! Stripe, as a payment provider.
//!
//! The platform holds one Stripe secret key (`SOAT_STRIPE_SECRET_KEY`);
//! each shop's payment method may name its own connected account
//! (`account_id`, Stripe Connect), so no shop secret is ever stored.
//! Starting a payment creates a PaymentIntent and hands the buyer its
//! client secret once; Stripe's signed webhooks
//! (`POST /api/payments/stripe/webhook`, `SOAT_STRIPE_WEBHOOK_SECRET`)
//! move the payment along.
//!
//! Method settings: `account_id` (`acct_...`, optional),
//! `publishable_key` (`pk_...`, for the storefront) and `capture`
//! (`automatic`, the default, or `manual` to authorize now and capture
//! later).

mod api;
pub mod signature;
mod webhook;

use std::sync::Arc;

use serde_json::{json, Value};
use soat_module::Module;
use soat_payments::{
    PaymentProvider, PaymentStatus, ProviderFuture, ProviderPayment, ProviderUpdate, StartRequest,
};
use zapline::prelude::*;

/// How this server reaches Stripe.
#[derive(Clone)]
pub struct StripeConfig {
    /// `sk_live_...` or `sk_test_...`.
    pub secret_key: String,
    /// `whsec_...`: checks webhook events.
    pub webhook_secret: String,
    /// `https://api.stripe.com`; tests point it at a local fake.
    pub api_base: String,
}

impl StripeConfig {
    /// From `SOAT_STRIPE_SECRET_KEY`, `SOAT_STRIPE_WEBHOOK_SECRET` and
    /// optionally `SOAT_STRIPE_API_BASE`; `None` without a secret key.
    pub fn from_env() -> Option<Self> {
        let secret_key = std::env::var("SOAT_STRIPE_SECRET_KEY").ok()?;
        Some(Self {
            secret_key,
            webhook_secret: std::env::var("SOAT_STRIPE_WEBHOOK_SECRET").unwrap_or_default(),
            api_base: std::env::var("SOAT_STRIPE_API_BASE")
                .unwrap_or_else(|_| "https://api.stripe.com".to_owned()),
        })
    }
}

/// The Stripe payment provider; register it with
/// `Payments::new().with(Stripe::new(config.clone()))` and add
/// [`StripeWebhooks`] for its events.
pub struct Stripe {
    config: Arc<StripeConfig>,
}

impl Stripe {
    pub fn new(config: StripeConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

fn account(settings: &Value) -> Option<&str> {
    settings.get("account_id").and_then(Value::as_str)
}

/// What a PaymentIntent's status means for the payment.
pub fn intent_update(pi: &Value) -> ProviderUpdate {
    let status = match pi["status"].as_str().unwrap_or_default() {
        "succeeded" => PaymentStatus::Paid,
        "requires_capture" => PaymentStatus::Authorized,
        "canceled" => PaymentStatus::Canceled,
        "processing" => PaymentStatus::Pending,
        _ => PaymentStatus::RequiresAction,
    };
    ProviderUpdate {
        provider_ref: pi["id"].as_str().map(str::to_owned),
        failure_reason: pi["last_payment_error"]["message"]
            .as_str()
            .map(str::to_owned),
        ..ProviderUpdate::status(status)
    }
}

impl PaymentProvider for Stripe {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn label(&self) -> &'static str {
        "Stripe: cards, wallets, SEPA, iDEAL, Bancontact and more"
    }

    fn check_settings(&self, settings: &Value) -> Result<()> {
        let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_settings"));
        let Some(o) = settings.as_object() else {
            return bad("settings are an object");
        };
        for (k, v) in o {
            let s = v.as_str().unwrap_or_default();
            let ok = match k.as_str() {
                "account_id" => s.starts_with("acct_") && s.len() <= 64,
                "publishable_key" => s.starts_with("pk_") && s.len() <= 255,
                "capture" => matches!(s, "automatic" | "manual"),
                _ => false,
            };
            if !ok {
                return bad(&format!(
                    "{k} is not a Stripe setting here: use account_id (acct_), \
                     publishable_key (pk_) or capture (automatic or manual); \
                     secret keys belong in the server's environment"
                ));
            }
        }
        Ok(())
    }

    fn start<'a>(&'a self, req: &'a StartRequest<'a>) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let capture = req.settings["capture"].as_str().unwrap_or("automatic");
            let mut pairs = vec![
                ("amount", req.amount_minor.to_string()),
                ("currency", req.currency.to_ascii_lowercase()),
                ("automatic_payment_methods[enabled]", "true".to_owned()),
                ("capture_method", capture.to_owned()),
                ("metadata[soat_tenant]", req.tenant_id.to_owned()),
                ("metadata[soat_payment]", req.payment_id.to_owned()),
                ("metadata[soat_reference]", req.reference.to_owned()),
            ];
            if let Some(url) = req.return_url {
                pairs.push(("metadata[soat_return_url]", url.to_owned()));
            }
            let pi = api::post(
                &self.config,
                "/v1/payment_intents",
                &pairs,
                account(req.settings),
                Some(req.payment_id),
            )
            .await?;
            let mut update = intent_update(&pi);
            update.failure_reason = None;
            update.next_action = Some(json!({
                "type": "stripe",
                "publishable_key": req.settings.get("publishable_key"),
                "account_id": account(req.settings),
            }));
            update.buyer_secret = pi["client_secret"].as_str().map(str::to_owned);
            Ok(update)
        })
    }

    fn capture<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
    ) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let pi = payment.provider_ref.ok_or_else(no_intent)?;
            let pi = api::post(
                &self.config,
                &format!("/v1/payment_intents/{pi}/capture"),
                &[],
                account(payment.settings),
                Some(&format!("{}-capture", payment.payment_id)),
            )
            .await?;
            Ok(intent_update(&pi))
        })
    }

    fn cancel<'a>(&'a self, payment: &'a ProviderPayment<'a>) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            // A payment Stripe never heard of has nothing to cancel there.
            let Some(pi) = payment.provider_ref else {
                return Ok(());
            };
            api::post(
                &self.config,
                &format!("/v1/payment_intents/{pi}/cancel"),
                &[],
                account(payment.settings),
                Some(&format!("{}-cancel", payment.payment_id)),
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
            let pi = payment.provider_ref.ok_or_else(no_intent)?;
            let refund = api::post(
                &self.config,
                "/v1/refunds",
                &[
                    ("payment_intent", pi.to_owned()),
                    ("amount", amount_minor.to_string()),
                    ("metadata[soat_payment]", payment.payment_id.to_owned()),
                ],
                account(payment.settings),
                None,
            )
            .await?;
            Ok(refund["id"].as_str().map(str::to_owned))
        })
    }
}

fn no_intent() -> Error {
    Error::conflict("Stripe has no PaymentIntent for this payment yet").with_code("not_started")
}

/// Receives Stripe's webhook events.
pub struct StripeWebhooks {
    config: Arc<StripeConfig>,
}

impl StripeWebhooks {
    pub fn new(config: StripeConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl Module for StripeWebhooks {
    fn name(&self) -> &'static str {
        "stripe"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "payments"]
    }

    fn mount(&self, app: App) -> App {
        app.state(webhook::WebhookSecret(self.config.webhook_secret.clone()))
            .plugin(webhook::mount)
    }
}
