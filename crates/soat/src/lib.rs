//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! This crate holds no domain code. Each domain is a `soat-*` crate exporting
//! a `Module`; [`modules`] lists the ones this build includes, in dependency
//! order, and [`app`] wires them into one Zapline app.

pub use soat_auth::Auth;
use soat_cart::Carts;
use soat_catalog::Catalog;
use soat_customers::Customers;
use soat_events::Events;
use soat_fields::Fields;
use soat_files::Files;
use soat_inventory::Inventory;
use soat_module::Modules;
use soat_orders::Orders;
use soat_payments::Payments;
use soat_pricing::Pricing;
use soat_shipping::Shipping;
use soat_tax::Taxes;
use soat_tenancy::Tenancy;
use soat_webhooks::Webhooks;
use zapline::db::{Database, DbCheck};
use zapline::middleware::SecurityHeaders;
use zapline::openapi::OpenApi;
use zapline::prelude::*;
use zapline::storage::Storage;

/// What a SOAT install is configured with.
pub struct Settings {
    pub auth: Auth,
    /// Where uploaded files go.
    pub storage: Storage,
    /// Stripe, when configured.
    #[cfg(feature = "stripe")]
    pub stripe: Option<soat_stripe::StripeConfig>,
}

impl Settings {
    /// From the environment: `SOAT_SETUP_TOKEN`, and `STORAGE_DIR`
    /// (default `./storage`), or `S3_BUCKET` and friends, and
    /// `STORAGE_SECRET` to keep file links valid across restarts;
    /// `SOAT_STRIPE_SECRET_KEY` and `SOAT_STRIPE_WEBHOOK_SECRET` for Stripe.
    ///
    /// # Errors
    ///
    /// When the storage settings are inconsistent.
    pub fn from_env() -> zapline::Result<Self> {
        Ok(Self {
            auth: Auth::from_env(),
            storage: Storage::from_env()?,
            #[cfg(feature = "stripe")]
            stripe: soat_stripe::StripeConfig::from_env(),
        })
    }

    /// For tests: a fixed setup token and files kept in memory.
    pub fn for_tests(setup_token: &str) -> Self {
        Self {
            auth: Auth::with_setup_token(setup_token),
            storage: Storage::memory(),
            #[cfg(feature = "stripe")]
            stripe: None,
        }
    }
}

/// Every module in this build. A module must come after the ones it needs.
pub fn modules(settings: Settings) -> Modules {
    #[allow(unused_mut)]
    let mut payments = Payments::new();
    #[cfg(feature = "stripe")]
    if let Some(config) = &settings.stripe {
        payments = payments.with(soat_stripe::Stripe::new(config.clone()));
    }
    let modules = Modules::new()
        .with(settings.auth)
        .with(Events::new())
        .with(Tenancy)
        .with(Webhooks::new())
        .with(Fields)
        .with(Files::new(settings.storage))
        .with(Catalog)
        .with(Inventory)
        .with(Pricing)
        .with(Customers)
        .with(Carts)
        .with(Taxes)
        .with(Shipping)
        .with(payments)
        .with(Orders);
    #[cfg(feature = "stripe")]
    let modules = match settings.stripe {
        Some(config) => modules.with(soat_stripe::StripeWebhooks::new(config)),
        None => modules,
    };
    modules
}

/// Builds the application on top of `db` (Postgres). `main` passes the real
/// database and `Settings::from_env()`; tests pass a fresh schema each and
/// `Settings::for_tests` (see `tests/common`).
///
/// # Panics
///
/// When the module list is inconsistent (a missing dependency, a duplicate
/// name or migration version). That is a build mistake, caught by tests.
pub fn app(db: Database, settings: Settings) -> App {
    let app = App::new()
        .wrap(SecurityHeaders::default())
        .plugin(Health::new().check("database", DbCheck::new()))
        .openapi(OpenApi::new("SOAT", env!("CARGO_PKG_VERSION")));
    modules(settings).build(app, db)
}
