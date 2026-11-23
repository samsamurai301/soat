//! Payments.
//!
//! A [`PaymentProvider`] talks to one way of taking money; the server
//! registers its providers on the module ([`Payments::with`]), and each
//! shop sets up payment methods using them. A payment collects an amount
//! for a reference (an order, a cart) through one method and moves
//! through [`PaymentStatus`]; refunds return some or all of it.
//!
//! The built-in [`Manual`] provider covers bank transfer, cash on
//! delivery and invoice: the buyer sees the method's instructions and
//! staff mark the payment paid. Stripe comes in C8; its webhooks will feed
//! [`apply`].

pub mod http;
mod methods;
mod payments;
mod provider;

use std::sync::Arc;

use soat_core::Permission;
use soat_module::Module;
use zapline::embed::Dir;
use zapline::prelude::*;

pub use methods::{active_method, PaymentMethod, PaymentMethodId};
pub use payments::{
    apply, for_reference, get, refund, refund_reference, start, Payment, PaymentId, Refund,
    RefundResult, StartPayment,
};
pub use provider::{
    Manual, PaymentProvider, ProviderFuture, ProviderPayment, ProviderUpdate, Providers,
    StartRequest,
};
pub use soat_core::payment::PaymentStatus;

/// See payments and payment methods.
pub const PAYMENTS_READ: Permission =
    Permission::new("payments:read", "See payments, refunds and payment methods").for_staff();
/// Start payments, mark them paid, capture and cancel them.
pub const PAYMENTS_WRITE: Permission = Permission::new(
    "payments:write",
    "Start payments, mark them paid, capture and cancel them",
)
.for_staff();
/// Refund payments. Not given to staff by default.
pub const PAYMENTS_REFUND: Permission = Permission::new("payments:refund", "Refund payments");
/// Set up payment methods. Not given to staff by default.
pub const PAYMENTS_MANAGE: Permission =
    Permission::new("payments:manage", "Set up the shop's payment methods");

/// The payments module.
pub struct Payments {
    providers: Vec<Arc<dyn PaymentProvider>>,
}

impl Payments {
    /// With the [`Manual`] provider only.
    pub fn new() -> Self {
        Self {
            providers: vec![Arc::new(Manual)],
        }
    }

    /// Adds a provider; one with the same name replaces it.
    pub fn with(mut self, provider: impl PaymentProvider + 'static) -> Self {
        self.providers.retain(|p| p.name() != provider.name());
        self.providers.push(Arc::new(provider));
        self
    }
}

impl Default for Payments {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Payments {
    fn name(&self) -> &'static str {
        "payments"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[
            PAYMENTS_READ,
            PAYMENTS_WRITE,
            PAYMENTS_REFUND,
            PAYMENTS_MANAGE,
        ]
    }

    fn mount(&self, app: App) -> App {
        app.state(Providers::new(self.providers.clone()))
            .plugin(methods::mount)
            .plugin(payments::mount)
    }
}
