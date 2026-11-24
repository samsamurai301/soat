//! Runs the `loyalty` fixture (fixtures/loyalty, built by fixtures/build.sh).

use std::collections::HashMap;

use soat_core::hooks::{Hook, HookCart, HookLine, Hooks, Verdict};
use soat_plugin_host::{Plugin, PluginHost};

const WASM: &[u8] = include_bytes!("../fixtures/loyalty.wasm");
const MANIFEST: &str = r#"{"name":"loyalty","version":"1.0.0","world":"discounts","api":"0.1.0"}"#;

fn line(tags: &[&str], total: i64) -> HookLine {
    HookLine {
        item_id: "item".into(),
        sku: "SKU".into(),
        title: "Thing".into(),
        quantity: 1,
        unit_price_minor: total,
        line_total_minor: total,
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

fn cart() -> HookCart {
    HookCart {
        currency: "EUR".into(),
        country: Some("DE".into()),
        customer_tags: vec![],
        lines: vec![line(&["plugin"], 1000), line(&[], 500)],
        subtotal_minor: 1500,
    }
}

fn loyalty(settings: &[(&str, &str)]) -> Plugin {
    let settings: HashMap<_, _> = settings
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    PluginHost::new()
        .unwrap()
        .load(MANIFEST, WASM, settings)
        .unwrap()
}

#[test]
fn a_plugin_discounts_with_its_settings_and_logs() {
    let plugin = loyalty(&[("amount", "250")]);
    let out = plugin.discount("shop", &cart());
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].line, out[0].amount_minor), (Some(0), 250));
    assert_eq!(out[0].label, "Loyalty");
    assert_eq!((out[1].line, out[1].amount_minor), (None, 15000));
    assert_eq!(plugin.log(), vec!["info: 2 lines seen".to_string()]);

    // Without the setting it takes its default.
    let out = loyalty(&[]).discount("shop", &cart());
    assert_eq!(out[0].amount_minor, 100);
}

#[test]
fn checkout_keeps_a_plugin_within_the_cart() {
    let hooks = Hooks::new().with(loyalty(&[("amount", "250")]));
    let out = hooks.discounts("shop", &cart());
    let total: i64 = out.iter().map(|a| a.amount_minor).sum();
    assert_eq!(total, 1500, "{out:?}");
    assert_eq!(out[1].amount_minor, 1250);
}

#[test]
fn a_plugin_out_of_fuel_changes_nothing_and_says_why() {
    let plugin = PluginHost::new()
        .unwrap()
        .with_fuel(10)
        .load(MANIFEST, WASM, HashMap::new())
        .unwrap();
    assert!(plugin.discount("shop", &cart()).is_empty());
    let log = plugin.log().join("\n");
    assert!(log.contains("fuel"), "{log}");
    // Other hook points pass it by.
    assert_eq!(plugin.validate("shop", &cart()), Verdict::Allow);
}

#[test]
fn a_plugin_is_checked_on_load() {
    let host = PluginHost::new().unwrap();
    let wrong = MANIFEST.replace("\"discounts\"", "\"shipping-filter\"");
    let err = host.load(&wrong, WASM, HashMap::new()).err().unwrap();
    assert!(err.contains("not a shipping-filter plugin"), "{err}");
    let err = host
        .load(MANIFEST, b"nonsense", HashMap::new())
        .err()
        .unwrap();
    assert!(err.contains("not a component"), "{err}");
    let err = host.load("{}", WASM, HashMap::new()).err().unwrap();
    assert!(!err.is_empty());
}

#[test]
fn a_folder_of_plugins_loads_in_name_order() {
    let dir = std::env::temp_dir().join(format!("soat-plugins-{}", std::process::id()));
    for (name, amount) in [("b-second", "20"), ("a-first", "10")] {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("soat.plugin.json"), MANIFEST).unwrap();
        std::fs::write(folder.join("plugin.wasm"), WASM).unwrap();
        std::fs::write(
            folder.join("settings.json"),
            format!(r#"{{"amount":"{amount}"}}"#),
        )
        .unwrap();
    }
    let plugins = PluginHost::new().unwrap().load_dir(&dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let firsts: Vec<i64> = plugins
        .iter()
        .map(|p| p.discount("shop", &cart())[0].amount_minor)
        .collect();
    assert_eq!(firsts, vec![10, 20]);
}
