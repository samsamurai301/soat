//! The provider trait every payment integration implements, and the
//! built-in manual provider.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};
use soat_core::payment::PaymentStatus;
use zapline::prelude::*;

/// What a provider call returns.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A payment to start with a provider.
#[derive(Debug, Clone)]
pub struct StartRequest<'a> {
    pub tenant_id: &'a str,
    /// `pay_...`: pass it to the provider as an idempotency key.
    pub payment_id: &'a str,
    pub reference: &'a str,
    pub amount_minor: i64,
    pub currency: &'a str,
    /// Where to send the buyer back after a redirect.
    pub return_url: Option<&'a str>,
    /// The payment method's settings.
    pub settings: &'a Value,
}

/// A payment as a provider sees it.
#[derive(Debug, Clone)]
pub struct ProviderPayment<'a> {
    pub tenant_id: &'a str,
    pub payment_id: &'a str,
    pub provider_ref: Option<&'a str>,
    pub amount_minor: i64,
    pub currency: &'a str,
    pub settings: &'a Value,
}

/// What a provider says about a payment.
#[derive(Debug, Clone)]
pub struct ProviderUpdate {
    pub status: PaymentStatus,
    /// The provider's own id, e.g. a Stripe PaymentIntent id.
    pub provider_ref: Option<String>,
    /// What the buyer must do next (a redirect URL, a client secret, bank
    /// details), shown by the storefront.
    pub next_action: Option<Value>,
    pub failure_reason: Option<String>,
    /// A secret only the buyer gets (e.g. a Stripe client secret): returned
    /// once by [`start`](crate::start), never stored or published.
    pub buyer_secret: Option<String>,
}

impl ProviderUpdate {
    pub fn status(status: PaymentStatus) -> Self {
        Self {
            status,
            provider_ref: None,
            next_action: None,
            failure_reason: None,
            buyer_secret: None,
        }
    }
}

/// A payment integration: manual, Stripe (C8), PayPal and Mollie (C18), or
/// a plugin's.
pub trait PaymentProvider: Send + Sync {
    /// Stored on methods and payments, e.g. `manual` or `stripe`.
    fn name(&self) -> &'static str;

    /// Shown to merchants, e.g. `Bank transfer or cash on delivery`.
    fn label(&self) -> &'static str;

    /// Whether staff confirm payments by hand (`mark-paid`), as with a bank
    /// transfer.
    fn confirmed_by_staff(&self) -> bool {
        false
    }

    /// Checks a method's settings before they are saved; never put secrets
    /// in them.
    fn check_settings(&self, _settings: &Value) -> Result<()> {
        Ok(())
    }

    /// Starts collecting a payment.
    fn start<'a>(&'a self, req: &'a StartRequest<'a>) -> ProviderFuture<'a, ProviderUpdate>;

    /// Collects an authorized payment.
    fn capture<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
    ) -> ProviderFuture<'a, ProviderUpdate> {
        let _ = payment;
        Box::pin(async {
            Err(Error::unprocessable("this provider cannot capture").with_code("not_supported"))
        })
    }

    /// Cancels a payment that was not collected.
    fn cancel<'a>(&'a self, payment: &'a ProviderPayment<'a>) -> ProviderFuture<'a, ()>;

    /// Returns `amount_minor` of a collected payment; gives the provider's
    /// refund id, if any.
    fn refund<'a>(
        &'a self,
        payment: &'a ProviderPayment<'a>,
        amount_minor: i64,
    ) -> ProviderFuture<'a, Option<String>>;
}

/// The providers this server has, by name.
#[derive(Clone, Default)]
pub struct Providers(Arc<BTreeMap<&'static str, Arc<dyn PaymentProvider>>>);

impl Providers {
    pub fn new(list: Vec<Arc<dyn PaymentProvider>>) -> Self {
        Self(Arc::new(list.into_iter().map(|p| (p.name(), p)).collect()))
    }

    /// The provider called `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn PaymentProvider>> {
        self.0.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn PaymentProvider>> {
        self.0.values()
    }
}

/// Bank transfer, cash on delivery, invoice: the buyer gets the method's
/// `instructions`; staff mark the payment paid when the money arrives.
pub struct Manual;

impl PaymentProvider for Manual {
    fn name(&self) -> &'static str {
        "manual"
    }

    fn label(&self) -> &'static str {
        "Manual: bank transfer, cash on delivery, invoice"
    }

    fn confirmed_by_staff(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> Result<()> {
        let ok = settings.as_object().is_some_and(|o| {
            o.iter().all(|(k, v)| {
                k == "instructions" && v.as_str().is_some_and(|s| s.chars().count() <= 2000)
            })
        });
        if ok {
            Ok(())
        } else {
            Err(Error::unprocessable(
                "manual settings take only `instructions`, a text of at most 2000 characters",
            )
            .with_code("invalid_settings"))
        }
    }

    fn start<'a>(&'a self, req: &'a StartRequest<'a>) -> ProviderFuture<'a, ProviderUpdate> {
        Box::pin(async move {
            let instructions = req
                .settings
                .get("instructions")
                .cloned()
                .unwrap_or(json!(""));
            Ok(ProviderUpdate {
                next_action: Some(json!({ "type": "instructions", "text": instructions })),
                ..ProviderUpdate::status(PaymentStatus::Pending)
            })
        })
    }

    fn cancel<'a>(&'a self, _: &'a ProviderPayment<'a>) -> ProviderFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn refund<'a>(
        &'a self,
        _: &'a ProviderPayment<'a>,
        _: i64,
    ) -> ProviderFuture<'a, Option<String>> {
        // The money goes back by hand; this records it.
        Box::pin(async { Ok(None) })
    }
}
