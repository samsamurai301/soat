//! SOAT's plugin interfaces, v0.
//!
//! A plugin is a WebAssembly component that targets one world of the
//! `soat:plugin` WIT package ([`WIT`], in `wit/soat-plugin.wit`): one per
//! hook point of `soat_core::hooks`. It ships with a `soat.plugin.json`
//! ([`PluginManifest`]) naming its world, the permissions it needs and the
//! settings a shop fills in. Plugins run with no network, files or clock
//! beyond what the `host` interface lends them.

use serde::{Deserialize, Serialize};
pub use soat_core::hooks::HOOKS as WORLDS;

/// The WIT package plugins target.
pub const WIT: &str = include_str!("../wit/soat-plugin.wit");

/// Its version, as the package line has it.
pub const VERSION: &str = "0.1.0";

/// A setting a shop fills in for a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingSpec {
    /// Lowercase letters, digits and `_`, e.g. `points_per_euro`.
    pub key: String,
    /// What the shop sees, e.g. `Points per euro`.
    pub label: String,
    /// Kept sealed, never shown again (API keys).
    #[serde(default)]
    pub secret: bool,
}

/// A plugin's `soat.plugin.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Lowercase letters, digits and `-`, e.g. `loyalty-discounts`.
    pub name: String,
    /// Semantic version, e.g. `1.2.0`.
    pub version: String,
    /// One of [`WORLDS`].
    pub world: String,
    /// The `soat:plugin` version it was built against, e.g. `0.1.0`.
    pub api: String,
    /// What it may read through the host, e.g. `products:read`.
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingSpec>,
}

fn is_name(s: &str, extra: char) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == extra)
}

fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit()))
}

impl PluginManifest {
    /// Reads and checks a manifest.
    pub fn parse(json: &str) -> Result<Self, Vec<String>> {
        let m: Self = serde_json::from_str(json).map_err(|e| vec![e.to_string()])?;
        m.check().map(|()| m)
    }

    /// Everything wrong with it; `known` says whether a permission exists.
    pub fn problems(&self, known: impl Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        if !is_name(&self.name, '-') {
            out.push(format!(
                "name {:?}: lowercase letters, digits and -",
                self.name
            ));
        }
        if !is_version(&self.version) {
            out.push(format!("version {:?}: like 1.2.0", self.version));
        }
        if !WORLDS.contains(&self.world.as_str()) {
            out.push(format!(
                "world {:?}: one of {}",
                self.world,
                WORLDS.join(", ")
            ));
        }
        // Before 1.0 a minor version breaks; the patch adds nothing plugins see.
        let line = |v: &str| v.rsplit_once('.').map(|(l, _)| l.to_owned());
        if !is_version(&self.api) || line(&self.api) != line(VERSION) {
            out.push(format!(
                "api {:?}: this SOAT offers soat:plugin {VERSION}",
                self.api
            ));
        }
        for p in &self.permissions {
            if !known(p) {
                out.push(format!("unknown permission {p:?}"));
            }
        }
        let mut keys = Vec::new();
        for s in &self.settings {
            if !is_name(&s.key, '_') || keys.contains(&&s.key) {
                out.push(format!(
                    "setting {:?}: unique, lowercase letters, digits and _",
                    s.key
                ));
            }
            if s.label.trim().is_empty() {
                out.push(format!("setting {:?} has no label", s.key));
            }
            keys.push(&s.key);
        }
        out
    }

    /// [`problems`](Self::problems) with permissions shaped `resource:action`.
    pub fn check(&self) -> Result<(), Vec<String>> {
        let shaped = |p: &str| {
            p.split_once(':')
                .is_some_and(|(r, a)| is_name(r, '_') && is_name(a, '_'))
        };
        let problems = self.problems(shaped);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soat_core::hooks::{Adjustment, HookCart, HookEvent, HookLine, HookShippingOption};
    use wit_parser::{Resolve, TypeDefKind, WorldItem};

    fn resolve() -> (Resolve, wit_parser::PackageId) {
        let mut r = Resolve::default();
        let pkg = r.push_str("soat-plugin.wit", WIT).expect("the WIT parses");
        (r, pkg)
    }

    #[test]
    fn the_package_is_versioned_and_has_a_world_per_hook() {
        let (r, pkg) = resolve();
        let p = &r.packages[pkg];
        assert_eq!(p.name.to_string(), format!("soat:plugin@{VERSION}"));
        let mut worlds: Vec<&str> = p.worlds.keys().map(String::as_str).collect();
        worlds.sort();
        let mut hooks = WORLDS.to_vec();
        hooks.sort();
        assert_eq!(worlds, hooks);
        for (name, id) in &p.worlds {
            let w = &r.worlds[*id];
            assert_eq!(w.exports.len(), 1, "{name} exports one function");
            assert!(
                w.imports
                    .values()
                    .any(|i| matches!(i, WorldItem::Interface { .. })),
                "{name} imports the host"
            );
        }
    }

    /// The WIT record's fields as Rust names, sorted.
    fn fields(r: &Resolve, name: &str) -> Vec<String> {
        let (_, ty) = r
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no WIT type {name}"));
        match &ty.kind {
            TypeDefKind::Record(rec) => rec
                .fields
                .iter()
                .map(|f| f.name.replace('-', "_"))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
            other => panic!("{name} is {other:?}"),
        }
    }

    /// The Rust record's fields, sorted.
    fn rust<T: Serialize>(value: &T) -> Vec<String> {
        let map = serde_json::to_value(value).unwrap();
        map.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn the_records_mirror_soat_core() {
        let (r, _) = resolve();
        let line = HookLine {
            item_id: String::new(),
            sku: String::new(),
            title: String::new(),
            quantity: 0,
            unit_price_minor: 0,
            line_total_minor: 0,
            tags: vec![],
        };
        let cart = HookCart {
            currency: String::new(),
            country: None,
            customer_tags: vec![],
            lines: vec![],
            subtotal_minor: 0,
        };
        let adjustment = Adjustment {
            line: None,
            amount_minor: 0,
            label: String::new(),
        };
        let option = HookShippingOption {
            rate_id: String::new(),
            name: String::new(),
            price_minor: 0,
        };
        let event = HookEvent {
            name: String::new(),
            payload: String::new(),
        };
        assert_eq!(fields(&r, "hook-line"), rust(&line));
        assert_eq!(fields(&r, "hook-cart"), rust(&cart));
        assert_eq!(fields(&r, "adjustment"), rust(&adjustment));
        assert_eq!(fields(&r, "hook-shipping-option"), rust(&option));
        assert_eq!(fields(&r, "hook-event"), rust(&event));
    }

    #[test]
    fn manifests_are_checked() {
        let good = r#"{ "name": "loyalty", "version": "1.0.0", "world": "discounts",
            "api": "0.1.0", "permissions": ["customers:read"],
            "settings": [{ "key": "points_per_euro", "label": "Points per euro" }] }"#;
        assert!(PluginManifest::parse(good).is_ok());
        let bad = r#"{ "name": "Loyalty!", "version": "1.0", "world": "rockets",
            "api": "2.0.0", "permissions": ["everything"],
            "settings": [{ "key": "a", "label": "" }, { "key": "a", "label": "A" }] }"#;
        let problems = PluginManifest::parse(bad).unwrap_err();
        assert_eq!(problems.len(), 7, "{problems:#?}");
    }
}
