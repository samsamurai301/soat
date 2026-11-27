//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! This crate holds no domain code. Each domain is a `soat-*` crate exporting
//! a `Module`; [`modules`] lists the ones this build includes, in dependency
//! order, and [`app`] wires them into one Zapline app.

use soat_admin::Admin;
pub use soat_auth::Auth;
use soat_cart::Carts;
use soat_catalog::Catalog;
use soat_checkout::Checkout;
use soat_cms::Cms;
use soat_core::secret::Sealer;
use soat_customers::Customers;
use soat_events::Events;
use soat_fields::Fields;
use soat_files::Files;
use soat_giftcards::GiftCards;
use soat_i18n::I18n;
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
use soat_storefront::Storefront;
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
    /// Seals secrets shops store, such as their own Anthropic key; none
    /// when `SOAT_SECRET_KEY` is unset, and then shops cannot store them.
    pub sealer: Option<Sealer>,
    /// Native hooks this install runs at checkout (`soat_core::hooks`);
    /// none by default.
    pub hooks: soat_core::hooks::Hooks,
    /// Compiles the plugins shops upload: the plugin host with the
    /// `plugins` feature, otherwise none and uploads are refused.
    pub plugin_loader: Option<std::sync::Arc<dyn soat_core::hooks::PluginLoader>>,
    /// Where Anthropic's API is: `soat_mcp::ANTHROPIC_API` unless a test
    /// or a proxy says otherwise.
    pub anthropic_api: String,
    /// Stripe, when configured.
    #[cfg(feature = "stripe")]
    pub stripe: Option<soat_stripe::StripeConfig>,
    /// Mollie, when configured.
    #[cfg(feature = "mollie")]
    pub mollie: Option<soat_mollie::MollieConfig>,
    /// PayPal, when configured.
    #[cfg(feature = "paypal")]
    pub paypal: Option<soat_paypal::PayPalConfig>,
    /// Sendcloud labels; shops bring their own accounts.
    #[cfg(feature = "sendcloud")]
    pub sendcloud: Option<soat_sendcloud::Sendcloud>,
    /// Where Shopify stores are called for the sync before cutover: each
    /// store's own domain, unless a test or proxy says otherwise.
    #[cfg(feature = "shopify-sync")]
    pub shopify_api_base: Option<String>,
}

impl Settings {
    /// From the environment: `SOAT_SETUP_TOKEN`, and `STORAGE_DIR`
    /// (default `./storage`), or `S3_BUCKET` and friends, and
    /// `STORAGE_SECRET` to keep file links valid across restarts;
    /// `SOAT_STRIPE_SECRET_KEY` and `SOAT_STRIPE_WEBHOOK_SECRET` for Stripe;
    /// `SOAT_MOLLIE_API_KEY` and `SOAT_PUBLIC_URL` (for its webhooks) for Mollie;
    /// `SOAT_PAYPAL_CLIENT_ID`, `SOAT_PAYPAL_CLIENT_SECRET` and
    /// `SOAT_PAYPAL_WEBHOOK_ID` for PayPal; `SOAT_SENDCLOUD_API_BASE` to
    /// reach Sendcloud elsewhere; `SOAT_SHOPIFY_API_BASE` to reach Shopify
    /// stores elsewhere;
    /// `SMTP_URL` and `MAIL_FROM` for email (without `SMTP_URL`, a debug
    /// build keeps mail in the dev inbox at `/_dev/mail`);
    /// `SOAT_SECRET_KEY` (32 characters or more, kept across restarts) to
    /// seal the keys shops store; `SOAT_ANTHROPIC_API_BASE` to reach
    /// Anthropic's API elsewhere (a proxy); `SOAT_PLUGINS_DIR` for the
    /// WebAssembly plugins checkout runs (one folder each, see
    /// `docs/plugins.md`).
    ///
    /// # Errors
    ///
    /// When the storage settings are inconsistent, `SMTP_URL` is invalid
    /// or missing in a release build, `SOAT_SECRET_KEY` is too short, or a
    /// plugin in `SOAT_PLUGINS_DIR` does not load.
    pub fn from_env() -> zapline::Result<Self> {
        Ok(Self {
            auth: Auth::from_env(),
            storage: Storage::from_env()?,
            mailer: Mailer::from_env()?,
            mail_from: std::env::var("MAIL_FROM")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "no-reply@localhost".to_owned()),
            sealer: match std::env::var("SOAT_SECRET_KEY") {
                Ok(s) if !s.is_empty() => Some(
                    Sealer::from_secret(&s)
                        .map_err(|e| zapline::Error::internal(format!("SOAT_SECRET_KEY: {e}")))?,
                ),
                _ => None,
            },
            anthropic_api: std::env::var("SOAT_ANTHROPIC_API_BASE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| soat_mcp::ANTHROPIC_API.to_owned()),
            #[cfg(feature = "stripe")]
            stripe: soat_stripe::StripeConfig::from_env(),
            #[cfg(feature = "mollie")]
            mollie: soat_mollie::MollieConfig::from_env(),
            #[cfg(feature = "paypal")]
            paypal: soat_paypal::PayPalConfig::from_env(),
            #[cfg(feature = "sendcloud")]
            sendcloud: Some(soat_sendcloud::Sendcloud::from_env()),
            #[cfg(feature = "shopify-sync")]
            shopify_api_base: std::env::var("SOAT_SHOPIFY_API_BASE").ok(),
            hooks: hooks_from_env()?,
            plugin_loader: plugin_loader()?,
        })
    }

    /// For tests: a fixed setup token, files and email kept in memory.
    pub fn for_tests(setup_token: &str) -> Self {
        Self {
            auth: Auth::with_setup_token(setup_token),
            storage: Storage::memory(),
            mailer: Mailer::outbox(),
            mail_from: "shops@soat.test".to_owned(),
            sealer: Sealer::from_secret("the test server's secret, long enough to use").ok(),
            // Nothing listens here: tests that call Claude set a fake.
            anthropic_api: "http://127.0.0.1:9".to_owned(),
            #[cfg(feature = "stripe")]
            stripe: None,
            #[cfg(feature = "mollie")]
            mollie: None,
            #[cfg(feature = "paypal")]
            paypal: None,
            #[cfg(feature = "sendcloud")]
            sendcloud: None,
            // Nothing listens here: tests that sync set a fake.
            #[cfg(feature = "shopify-sync")]
            shopify_api_base: Some("http://127.0.0.1:9".to_owned()),
            hooks: soat_core::hooks::Hooks::new(),
            plugin_loader: plugin_loader().ok().flatten(),
        }
    }
}

/// Every module in this build. A module must come after the ones it needs.
/// The last two serve the others' API: `mcp` as tools for Claude, `admin`
/// as pages for staff.
pub fn modules(settings: Settings) -> Modules {
    let api = settings.clone();
    let admin = Admin::new(move |app| with_mcp(api, Events::manual()).install(app));
    with_mcp(settings, Events::new()).with(admin)
}

/// Every module but `admin`. Admin pages call this API, the assistant's
/// approvals included.
fn with_mcp(settings: Settings, events: Events) -> Modules {
    let api = settings.clone();
    let sealer = settings.sealer.clone();
    let anthropic = settings.anthropic_api.clone();
    let mcp = Mcp::new(move |app| shop_modules(api, Events::manual()).install(app))
        .with_sealer(sealer)
        .with_anthropic_api(&anthropic);
    shop_modules(settings, events).with(mcp)
}

/// Every module but `mcp` and `admin`.
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
    #[allow(unused_mut)]
    let mut shipping = Shipping::new().with_sealer(settings.sealer.clone());
    #[cfg(feature = "sendcloud")]
    if let Some(s) = &settings.sendcloud {
        shipping = shipping.with(s.clone());
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
        .with(Taxes::new().with_sealer(settings.sealer.clone()))
        .with(shipping)
        .with(payments)
        .with(Promotions)
        .with(GiftCards)
        .with(Orders)
        .with(
            match settings.plugin_loader.clone() {
                Some(loader) => Checkout::new().with_plugin_loader(loader),
                None => Checkout::new(),
            }
            .with_hooks(settings.hooks.clone())
            .with_sealer(settings.sealer.clone()),
        )
        .with(Invoices)
        .with(Notify::new(settings.mailer, settings.mail_from))
        .with(Cms)
        .with(I18n)
        .with(Themes);
    // Apps before the storefront, which shows the blocks shops enable.
    #[cfg(feature = "apps")]
    let modules = modules.with(soat_apps::Apps);
    #[cfg(feature = "reviews")]
    let modules = modules.with(soat_reviews::Reviews);
    #[cfg(feature = "wishlists")]
    let modules = modules.with(soat_wishlists::Wishlists);
    #[cfg(feature = "loyalty")]
    let modules = modules.with(soat_loyalty::Loyalty);
    #[cfg(feature = "analytics")]
    let modules = modules.with(soat_analytics::Analytics);
    #[cfg(feature = "marketing")]
    let modules = modules.with(soat_marketing::Marketing);
    #[cfg(feature = "subscriptions")]
    let modules = modules.with(soat_subscriptions::Subscriptions);
    let modules = modules.with(Storefront);
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
    #[cfg(feature = "shopify-sync")]
    let modules = modules.with(
        soat_import::Imports::new()
            .with_sealer(settings.sealer.clone())
            .with_api_base(settings.shopify_api_base.clone()),
    );
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

/// The plugins in `SOAT_PLUGINS_DIR`, in folder order, as checkout hooks.
#[cfg(feature = "plugins")]
fn hooks_from_env() -> zapline::Result<soat_core::hooks::Hooks> {
    let mut hooks = soat_core::hooks::Hooks::new();
    let Ok(dir) = std::env::var("SOAT_PLUGINS_DIR") else {
        return Ok(hooks);
    };
    let failed = |e: String| zapline::Error::internal(format!("SOAT_PLUGINS_DIR: {e}"));
    let host = soat_plugin_host::PluginHost::new().map_err(failed)?;
    for plugin in host.load_dir(std::path::Path::new(&dir)).map_err(failed)? {
        hooks = hooks.with(plugin);
    }
    Ok(hooks)
}

#[cfg(not(feature = "plugins"))]
fn hooks_from_env() -> zapline::Result<soat_core::hooks::Hooks> {
    Ok(soat_core::hooks::Hooks::new())
}

/// The plugin host, compiling the plugins shops upload.
#[cfg(feature = "plugins")]
#[allow(clippy::unnecessary_wraps)]
fn plugin_loader() -> zapline::Result<Option<std::sync::Arc<dyn soat_core::hooks::PluginLoader>>> {
    let host = soat_plugin_host::PluginHost::new()
        .map_err(|e| zapline::Error::internal(format!("plugin host: {e}")))?;
    Ok(Some(std::sync::Arc::new(host)))
}

#[cfg(not(feature = "plugins"))]
#[allow(clippy::unnecessary_wraps)]
fn plugin_loader() -> zapline::Result<Option<std::sync::Arc<dyn soat_core::hooks::PluginLoader>>> {
    Ok(None)
}
