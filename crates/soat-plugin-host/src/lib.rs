//! Runs WebAssembly plugins at SOAT's hook points.
//!
//! A plugin is a component built against one world of `soat:plugin`
//! (`soat-plugin-api`) with its `soat.plugin.json`. [`PluginHost`] loads
//! it and gives back a [`Plugin`], which is a `soat_core::hooks::Hook`:
//! add it to the install's `Hooks` and checkout runs it, for the shops
//! that turn it on, with each shop's settings.
//!
//! Every call gets a fresh instance with a fuel budget and a memory cap,
//! and nothing but the `host` interface (its log and its settings): no
//! network, files or clock. A plugin that traps, runs out of fuel or
//! answers nonsense changes nothing, and its log says why.
//!
//! Zapline follow-up: the generic part (engine, limits, loading) belongs in
//! Zapline once it settles.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use soat_core::hooks as core;
use soat_plugin_api::PluginManifest;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

mod discounts {
    wasmtime::component::bindgen!({
        path: "../soat-plugin-api/wit",
        world: "discounts",
    });
}

mod shipping {
    wasmtime::component::bindgen!({
        path: "../soat-plugin-api/wit",
        world: "shipping-filter",
        with: {
            "soat:plugin/types": crate::discounts::soat::plugin::types,
            "soat:plugin/host": crate::discounts::soat::plugin::host,
        },
    });
}

mod validation {
    wasmtime::component::bindgen!({
        path: "../soat-plugin-api/wit",
        world: "checkout-validation",
        with: {
            "soat:plugin/types": crate::discounts::soat::plugin::types,
            "soat:plugin/host": crate::discounts::soat::plugin::host,
        },
    });
}

mod events {
    wasmtime::component::bindgen!({
        path: "../soat-plugin-api/wit",
        world: "events",
        with: {
            "soat:plugin/types": crate::discounts::soat::plugin::types,
            "soat:plugin/host": crate::discounts::soat::plugin::host,
        },
    });
}

use discounts::soat::plugin::host as wit_host;
use discounts::soat::plugin::types as wit;

/// Instructions one call may run.
pub const FUEL: u64 = 50_000_000;
/// Memory one instance may use.
pub const MEMORY_BYTES: usize = 32 << 20;
/// Lines of log a plugin keeps.
const LOG_LINES: usize = 200;

/// What an instance sees: the plugin's settings and its log.
struct Data {
    settings: Arc<HashMap<String, String>>,
    log: Arc<Mutex<VecDeque<String>>>,
    limits: StoreLimits,
}

fn push_log(log: &Mutex<VecDeque<String>>, line: String) {
    if let Ok(mut log) = log.lock() {
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }
}

impl wit::Host for Data {}

impl wit_host::Host for Data {
    fn log(&mut self, level: wit::Level, message: String) {
        let level = match level {
            wit::Level::Debug => "debug",
            wit::Level::Info => "info",
            wit::Level::Warn => "warn",
            wit::Level::Error => "error",
        };
        let message: String = message.chars().take(1000).collect();
        push_log(&self.log, format!("{level}: {message}"));
    }

    fn setting(&mut self, key: String) -> Option<String> {
        self.settings.get(&key).cloned()
    }
}

/// Loads plugins; one per install.
#[derive(Clone)]
pub struct PluginHost {
    engine: Engine,
    fuel: u64,
}

enum Kind {
    Discounts(discounts::DiscountsPre<Data>),
    Shipping(shipping::ShippingFilterPre<Data>),
    Validation(validation::CheckoutValidationPre<Data>),
    Events(events::EventsPre<Data>),
}

/// A loaded plugin: a hook a shop turns on. Cheap to clone; clones
/// share the component and the log.
#[derive(Clone)]
pub struct Plugin {
    inner: Arc<Loaded>,
    settings: Arc<HashMap<String, String>>,
}

struct Loaded {
    manifest: PluginManifest,
    engine: Engine,
    fuel: u64,
    kind: Kind,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl PluginHost {
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.wasm_component_model(true).consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        Ok(Self { engine, fuel: FUEL })
    }

    /// Instructions each call may run, instead of [`FUEL`].
    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// Loads a plugin from its manifest (JSON), its component and the
    /// settings the install gives it.
    pub fn load(
        &self,
        manifest: &str,
        wasm: &[u8],
        settings: HashMap<String, String>,
    ) -> Result<Plugin, String> {
        let manifest = PluginManifest::parse(manifest).map_err(|p| p.join("; "))?;
        let component = Component::new(&self.engine, wasm)
            .map_err(|e| format!("{}: not a component: {e}", manifest.name))?;
        let mut linker: Linker<Data> = Linker::new(&self.engine);
        wit_host::add_to_linker::<Data, HasSelf<Data>>(&mut linker, |d| d)
            .map_err(|e| e.to_string())?;
        wit::add_to_linker::<Data, HasSelf<Data>>(&mut linker, |d| d).map_err(|e| e.to_string())?;
        let pre = linker
            .instantiate_pre(&component)
            .map_err(|e| format!("{}: it needs more than the host lends: {e}", manifest.name))?;
        let wrong =
            |e: wasmtime::Error| format!("{}: not a {} plugin: {e}", manifest.name, manifest.world);
        let kind = match manifest.world.as_str() {
            "discounts" => Kind::Discounts(discounts::DiscountsPre::new(pre).map_err(wrong)?),
            "shipping-filter" => {
                Kind::Shipping(shipping::ShippingFilterPre::new(pre).map_err(wrong)?)
            }
            "checkout-validation" => {
                Kind::Validation(validation::CheckoutValidationPre::new(pre).map_err(wrong)?)
            }
            "events" => Kind::Events(events::EventsPre::new(pre).map_err(wrong)?),
            other => return Err(format!("{}: no world {other}", manifest.name)),
        };
        Ok(Plugin {
            inner: Arc::new(Loaded {
                manifest,
                engine: self.engine.clone(),
                fuel: self.fuel,
                kind,
                log: Arc::new(Mutex::new(VecDeque::new())),
            }),
            settings: Arc::new(settings),
        })
    }

    /// Loads every plugin in `dir`: one folder each, holding
    /// `soat.plugin.json`, `plugin.wasm` and, if it has settings,
    /// `settings.json` (an object of strings). Folders are loaded in name
    /// order, which is the order the hooks run in.
    pub fn load_dir(&self, dir: &Path) -> Result<Vec<Plugin>, String> {
        let mut folders: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        folders.sort();
        let mut out = Vec::new();
        for folder in folders {
            let read = |name: &str| {
                std::fs::read(folder.join(name))
                    .map_err(|e| format!("{}: {e}", folder.join(name).display()))
            };
            let manifest = String::from_utf8(read("soat.plugin.json")?)
                .map_err(|_| format!("{}: not UTF-8", folder.display()))?;
            let settings = match read("settings.json") {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}/settings.json: {e}", folder.display()))?,
                Err(_) => HashMap::new(),
            };
            out.push(self.load(&manifest, &read("plugin.wasm")?, settings)?);
        }
        Ok(out)
    }
}

/// Largest component a shop may upload.
pub const MAX_UPLOAD_BYTES: usize = 8 << 20;

/// Plugins a shop uploads: checkout worlds only (`events` plugins run for
/// the whole install and come from `SOAT_PLUGINS_DIR`).
impl core::PluginLoader for PluginHost {
    fn load(&self, manifest: &str, wasm: &[u8]) -> Result<Arc<dyn core::Hook>, String> {
        if wasm.len() > MAX_UPLOAD_BYTES {
            return Err(format!(
                "the component is {} bytes; at most {MAX_UPLOAD_BYTES}",
                wasm.len()
            ));
        }
        let plugin = PluginHost::load(self, manifest, wasm, HashMap::new())?;
        if plugin.manifest().world == "events" {
            return Err(format!(
                "{}: events plugins cannot be uploaded by a shop",
                plugin.manifest().name
            ));
        }
        Ok(Arc::new(plugin))
    }
}

fn to_wit_line(l: &core::HookLine) -> wit::HookLine {
    wit::HookLine {
        item_id: l.item_id.clone(),
        sku: l.sku.clone(),
        title: l.title.clone(),
        quantity: l.quantity,
        unit_price_minor: l.unit_price_minor,
        line_total_minor: l.line_total_minor,
        tags: l.tags.clone(),
    }
}

fn to_wit_cart(c: &core::HookCart) -> wit::HookCart {
    wit::HookCart {
        currency: c.currency.clone(),
        country: c.country.clone(),
        customer_tags: c.customer_tags.clone(),
        lines: c.lines.iter().map(to_wit_line).collect(),
        subtotal_minor: c.subtotal_minor,
    }
}

fn to_wit_option(o: &core::HookShippingOption) -> wit::HookShippingOption {
    wit::HookShippingOption {
        rate_id: o.rate_id.clone(),
        name: o.name.clone(),
        price_minor: o.price_minor,
    }
}

fn from_wit_option(o: wit::HookShippingOption) -> core::HookShippingOption {
    core::HookShippingOption {
        rate_id: o.rate_id,
        name: o.name,
        price_minor: o.price_minor,
    }
}

impl Plugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.inner.manifest
    }

    /// The plugin's log, oldest first: what it wrote, and why calls failed.
    pub fn log(&self) -> Vec<String> {
        self.inner
            .log
            .lock()
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn store(&self) -> Store<Data> {
        let mut store = Store::new(
            &self.inner.engine,
            Data {
                settings: self.settings.clone(),
                log: self.inner.log.clone(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(MEMORY_BYTES)
                    .instances(16)
                    .memories(4)
                    .tables(16)
                    .build(),
            },
        );
        store.limiter(|d| &mut d.limits);
        // A fresh store always takes fuel.
        let _ = store.set_fuel(self.inner.fuel);
        store
    }

    /// Runs `call`; a failure is logged and gives `None`.
    fn run<T>(&self, call: impl FnOnce(&mut Store<Data>) -> wasmtime::Result<T>) -> Option<T> {
        let mut store = self.store();
        match call(&mut store) {
            Ok(v) => Some(v),
            Err(e) => {
                push_log(&self.inner.log, format!("error: the call failed: {e:#}"));
                None
            }
        }
    }

    /// Delivers an event to an `events` plugin; its error, if any.
    pub fn on_event(&self, event: &core::HookEvent) -> Result<(), String> {
        let Kind::Events(pre) = &self.inner.kind else {
            return Ok(());
        };
        let event = wit::HookEvent {
            name: event.name.clone(),
            payload: event.payload.clone(),
        };
        self.run(|s| pre.instantiate(&mut *s)?.call_on_event(&mut *s, &event))
            .unwrap_or_else(|| Err("the call failed".into()))
    }
}

impl core::Hook for Plugin {
    fn name(&self) -> &str {
        &self.inner.manifest.name
    }

    fn per_shop(&self) -> bool {
        true
    }

    fn settings(&self) -> Vec<core::HookSetting> {
        self.inner
            .manifest
            .settings
            .iter()
            .map(|s| core::HookSetting {
                key: s.key.clone(),
                label: s.label.clone(),
                secret: s.secret,
            })
            .collect()
    }

    /// The shop's settings over those the install gave it.
    fn with_settings(&self, settings: &HashMap<String, String>) -> Option<Arc<dyn core::Hook>> {
        let mut merged = (*self.settings).clone();
        merged.extend(settings.iter().map(|(k, v)| (k.clone(), v.clone())));
        Some(Arc::new(Plugin {
            inner: self.inner.clone(),
            settings: Arc::new(merged),
        }))
    }

    fn discount(&self, _shop: &str, cart: &core::HookCart) -> Vec<core::Adjustment> {
        let Kind::Discounts(pre) = &self.inner.kind else {
            return Vec::new();
        };
        let cart = to_wit_cart(cart);
        self.run(|s| pre.instantiate(&mut *s)?.call_discount(&mut *s, &cart))
            .unwrap_or_default()
            .into_iter()
            .map(|a| core::Adjustment {
                line: a.line,
                amount_minor: a.amount_minor,
                label: a.label,
            })
            .collect()
    }

    fn filter_shipping(
        &self,
        _shop: &str,
        cart: &core::HookCart,
        options: &[core::HookShippingOption],
    ) -> Vec<core::HookShippingOption> {
        let Kind::Shipping(pre) = &self.inner.kind else {
            return options.to_vec();
        };
        let cart = to_wit_cart(cart);
        let given: Vec<_> = options.iter().map(to_wit_option).collect();
        match self.run(|s| {
            pre.instantiate(&mut *s)?
                .call_filter(&mut *s, &cart, &given)
        }) {
            Some(kept) => kept.into_iter().map(from_wit_option).collect(),
            None => options.to_vec(),
        }
    }

    /// A plugin that fails lets the order through: its log says why.
    fn validate(&self, _shop: &str, cart: &core::HookCart) -> core::Verdict {
        let Kind::Validation(pre) = &self.inner.kind else {
            return core::Verdict::Allow;
        };
        let cart = to_wit_cart(cart);
        match self.run(|s| pre.instantiate(&mut *s)?.call_validate(&mut *s, &cart)) {
            Some(wit::Verdict::Block(reason)) => core::Verdict::Block(reason),
            _ => core::Verdict::Allow,
        }
    }
}
