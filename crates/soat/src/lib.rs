//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! This crate holds no domain code. Each domain is a `soat-*` crate exporting
//! a `Module`; [`modules`] lists the ones this build includes, in dependency
//! order, and [`app`] wires them into one Zapline app.

pub use soat_auth::Auth;
use soat_cart::Carts;
use soat_catalog::Catalog;
use soat_checkout::Checkout;
use soat_cms::Cms;
use soat_customers::Customers;
use soat_events::Events;
use soat_fields::Fields;
use soat_files::Files;
use soat_inventory::Inventory;
use soat_invoices::Invoices;
use soat_mcp::Mcp;
use soat_module::Modules;
use soat_notify::Notify;
use soat_orders::Orders;
use soat_payments::Payments;
use soat_pricing::Pricing;
use soat_promotions::Promotions;
use soat_shipping::Shipping;
use soat_tax::Taxes;
use soat_tenancy::Tenancy;
use soat_theme::Themes;
use soat_webhooks::Webhooks;
use zapline::db::{Database, DbCheck};
use zapline::mail::Mailer;
use zapline::middleware::SecurityHeaders;
use zapline::openapi::OpenApi;
use zapline::prelude::*;
use zapline::storage::Storage;

/// What a SOAT install is configured with.
#[derive(Clone)]
pub struct Settings {
    pub auth: Auth,
    /// Where uploaded files go.
    pub storage: Storage,
    /// Sends transactional email.
    pub mailer: Mailer,
    /// The address shops' email is sent from, with each shop's name.
    pub mail_from: String,
    /// Stripe, when configured.
    #[cfg(feature = "stripe")]
    pub stripe: Option<soat_stripe::StripeConfig>,
    /// Mollie, when configured.
    #[cfg(feature = "mollie")]
    pub mollie: Option<soat_mollie::MollieConfig>,
    /// PayPal, when configured.
    #[cfg(feature = "paypal")]
    pub paypal: Option<soat_paypal::PayPalConfig>,
}

impl Settings {
    /// From the environment: `SOAT_SETUP_TOKEN`, and `STORAGE_DIR`
    /// (default `./storage`), or `S3_BUCKET` and friends, and
    /// `STORAGE_SECRET` to keep file links valid across restarts;
    /// `SOAT_STRIPE_SECRET_KEY` and `SOAT_STRIPE_WEBHOOK_SECRET` for Stripe;
    /// `SOAT_MOLLIE_API_KEY` and `SOAT_PUBLIC_URL` (for its webhooks) for Mollie;
    /// `SOAT_PAYPAL_CLIENT_ID`, `SOAT_PAYPAL_CLIENT_SECRET` and
    /// `SOAT_PAYPAL_WEBHOOK_ID` for PayPal;
    /// `SMTP_URL` and `MAIL_FROM` for email (without `SMTP_URL`, a debug
    /// build keeps mail in the dev inbox at `/_dev/mail`).
    ///
    /// # Errors
    ///
    /// When the storage settings are inconsistent, or `SMTP_URL` is invalid
    /// or missing in a release build.
    pub fn from_env() -> zapline::Result<Self> {
        Ok(Self {
            auth: Auth::from_env(),
            storage: Storage::from_env()?,
            mailer: Mailer::from_env()?,
            mail_from: std::env::var("MAIL_FROM")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "no-reply@localhost".to_owned()),
            #[cfg(feature = "stripe")]
            stripe: soat_stripe::StripeConfig::from_env(),
            #[cfg(feature = "mollie")]
            mollie: soat_mollie::MollieConfig::from_env(),
            #[cfg(feature = "paypal")]
            paypal: soat_paypal::PayPalConfig::from_env(),
        })
    }

    /// For tests: a fixed setup token, files and email kept in memory.
    pub fn for_tests(setup_token: &str) -> Self {
        Self {
            auth: Auth::with_setup_token(setup_token),
            storage: Storage::memory(),
            mailer: Mailer::outbox(),
            mail_from: "shops@soat.test".to_owned(),
            #[cfg(feature = "stripe")]
            stripe: None,
            #[cfg(feature = "mollie")]
            mollie: None,
            #[cfg(feature = "paypal")]
            paypal: None,
        }
    }
}

/// Every module in this build. A module must come after the ones it needs.
/// The last, `mcp`, serves the others' API as tools for Claude.
pub fn modules(settings: Settings) -> Modules {
    let api = settings.clone();
    let mcp = Mcp::new(move |app| shop_modules(api, Events::manual()).install(app));
    shop_modules(settings, Events::new()).with(mcp)
}

/// Every module but `mcp`.
fn shop_modules(settings: Settings, events: Events) -> Modules {
    #[allow(unused_mut)]
    let mut payments = Payments::new();
    #[cfg(feature = "stripe")]
    if let Some(config) = &settings.stripe {
        payments = payments.with(soat_stripe::Stripe::new(config.clone()));
    }
    #[cfg(feature = "mollie")]
    if let Some(config) = &settings.mollie {
        payments = payments.with(soat_mollie::Mollie::new(config.clone()));
    }
    #[cfg(feature = "paypal")]
    let paypal = settings.paypal.clone().map(soat_paypal::PayPal::new);
    #[cfg(feature = "paypal")]
    if let Some(p) = &paypal {
        payments = payments.with(p.clone());
    }
    let modules = Modules::new()
        .with(settings.auth)
        .with(events)
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
        .with(Promotions)
        .with(Orders)
        .with(Checkout)
        .with(Invoices)
        .with(Notify::new(settings.mailer, settings.mail_from))
        .with(Cms)
        .with(Themes);
    #[cfg(feature = "stripe")]
    let modules = match settings.stripe {
        Some(config) => modules.with(soat_stripe::StripeWebhooks::new(config)),
        None => modules,
    };
    #[cfg(feature = "mollie")]
    let modules = match settings.mollie {
        Some(config) => modules.with(soat_mollie::MollieWebhooks::new(config)),
        None => modules,
    };
    #[cfg(feature = "paypal")]
    let modules = match paypal {
        Some(p) => modules.with(p.webhooks()),
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
