//! The module list of this build is consistent.

#[test]
fn module_list_is_valid_and_ordered() {
    let modules = soat::modules(soat::Settings::for_tests("t"));
    assert_eq!(modules.validate(), Ok(()));
    assert_eq!(
        modules.names(),
        [
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
            "orders",
            "checkout",
            "invoices",
            "notify"
        ]
    );
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
