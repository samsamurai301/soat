//! The module list of this build is consistent.

#[test]
fn module_list_is_valid_and_ordered() {
    let modules = soat::modules(soat::Settings::for_tests("t"));
    assert_eq!(modules.validate(), Ok(()));
    #[allow(unused_mut)]
    let mut expected = vec![
        "auth",
        "events",
        "tenancy",
        "webhooks",
        "fields",
        "files",
        "catalog",
        "inventory",
        "pricing",
        "customers",
        "cart",
        "tax",
        "shipping",
        "payments",
        "promotions",
        "giftcards",
        "orders",
        "checkout",
        "invoices",
        "notify",
        "cms",
        "i18n",
        "theme",
    ];
    #[cfg(feature = "apps")]
    expected.push("apps");
    #[cfg(feature = "reviews")]
    expected.push("reviews");
    #[cfg(feature = "wishlists")]
    expected.push("wishlists");
    #[cfg(feature = "loyalty")]
    expected.push("loyalty");
    #[cfg(feature = "analytics")]
    expected.push("analytics");
    #[cfg(feature = "marketing")]
    expected.push("marketing");
    #[cfg(feature = "subscriptions")]
    expected.push("subscriptions");
    #[cfg(feature = "b2b")]
    expected.push("b2b");
    #[cfg(feature = "feeds")]
    expected.push("channels");
    expected.push("storefront");
    #[cfg(feature = "shopify-sync")]
    expected.push("import");
    expected.extend(["mcp", "admin"]);
    assert_eq!(modules.names(), expected);
}

#[test]
fn every_module_migration_is_merged() {
    let files = soat::modules(soat::Settings::for_tests("t"))
        .migrations()
        .unwrap()
        .files()
        .unwrap();
    assert!(files.contains(&"20261123000001_create_tenants.up.sql".to_owned()));
    assert!(files.contains(&"20261123000002_create_products.up.sql".to_owned()));
    // Each up migration has its down migration.
    for up in files.iter().filter(|f| f.ends_with(".up.sql")) {
        let down = up.replace(".up.sql", ".down.sql");
        assert!(files.contains(&down), "{up} has no {down}");
    }
}
