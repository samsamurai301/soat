//! A12: dependencies point down.
//!
//! Every workspace crate has a rank. A crate's normal and build
//! dependencies on other SOAT crates must have a lower rank, so siblings in
//! one layer (equal rank) never import each other and nothing imports a
//! layer above it. Foundation crates form a stack, one rank each.
//! Dev-dependencies are free: tests may pull in whatever they need.
//!
//! A new crate fails this test until it is given a rank here, from the
//! layer table in `docs/roadmap.md`.

use std::collections::HashMap;
use std::process::Command;

use serde_json::Value;

const RANKS: &[(&str, u32)] = &[
    // Foundation, bottom up.
    ("soat-core", 0),
    ("soat-module", 1),
    ("soat-db", 2),
    ("soat-auth", 3),
    ("soat-events", 4),
    ("soat-tenancy", 5),
    ("soat-webhooks", 6),
    ("soat-fields", 7),
    ("soat-files", 8),
    // Catalog.
    ("soat-catalog", 10),
    ("soat-inventory", 10),
    ("soat-pricing", 10),
    // Selling.
    ("soat-customers", 20),
    ("soat-cart", 20),
    ("soat-tax", 20),
    ("soat-shipping", 20),
    ("soat-payments", 20),
    ("soat-checkout", 20),
    ("soat-orders", 20),
    ("soat-promotions", 20),
    ("soat-notify", 20),
    // Content.
    ("soat-cms", 30),
    ("soat-i18n", 30),
    ("soat-seo", 30),
    ("soat-search", 30),
    ("soat-theme", 30),
    // Extensions.
    ("soat-apps", 40),
    ("soat-plugin-api", 40),
    ("soat-plugin-host", 40),
    ("soat-cli", 40),
    ("soat-onboarding", 40),
    ("soat-import", 40),
    ("soat-ai", 40),
    ("soat-analytics", 40),
    ("soat-marketing", 40),
    ("soat-subscriptions", 40),
    ("soat-b2b", 40),
    ("soat-channels", 40),
    ("soat-compliance", 40),
    // Surfaces.
    ("soat-storefront", 50),
    ("soat-admin", 50),
    ("soat-mcp", 50),
    // The binary lists every module.
    ("soat", 100),
];

/// Only ever a dev-dependency.
const TEST_ONLY: &[&str] = &["soat-testing"];

fn workspace() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn dependencies_point_down() {
    let ranks: HashMap<&str, u32> = RANKS.iter().copied().collect();
    let meta = workspace();
    let packages = meta["packages"].as_array().unwrap();
    let mut problems = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap();
        if TEST_ONLY.contains(&name) {
            continue;
        }
        let Some(&rank) = ranks.get(name) else {
            problems.push(format!("{name} has no layer: add it to RANKS"));
            continue;
        };
        for dep in package["dependencies"].as_array().unwrap() {
            let dep_name = dep["name"].as_str().unwrap();
            if !dep_name.starts_with("soat") || dep["kind"].as_str() == Some("dev") {
                continue;
            }
            if TEST_ONLY.contains(&dep_name) {
                problems.push(format!(
                    "{name} depends on {dep_name}, which is for tests only"
                ));
                continue;
            }
            match ranks.get(dep_name) {
                Some(&r) if r < rank => {}
                Some(_) => problems.push(format!(
                    "{name} depends on {dep_name}, a sibling or a higher layer; \
                     talk through events instead"
                )),
                None => problems.push(format!("{dep_name} has no layer: add it to RANKS")),
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_table_is_ordered_and_has_no_duplicates() {
    let mut seen = std::collections::HashSet::new();
    let mut last = 0;
    for (name, rank) in RANKS {
        assert!(seen.insert(name), "{name} listed twice");
        assert!(*rank >= last, "{name} is out of order");
        last = *rank;
    }
}
