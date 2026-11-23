//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! This crate holds no domain code. Each domain is a `soat-*` crate exporting
//! a `Module`; [`modules`] lists the ones this build includes, in dependency
//! order, and [`app`] wires them into one Zapline app.

use soat_catalog::Catalog;
use soat_module::Modules;
use soat_tenancy::Tenancy;
use zapline::db::{Database, DbCheck};
use zapline::middleware::SecurityHeaders;
use zapline::openapi::OpenApi;
use zapline::prelude::*;

/// Every module in this build. A module must come after the ones it needs.
pub fn modules() -> Modules {
    Modules::new().with(Tenancy).with(Catalog)
}

/// Builds the application on top of `db`. `main` passes the real database;
/// tests pass `Database::sqlite_memory()`.
///
/// # Panics
///
/// When the module list is inconsistent (a missing dependency, a duplicate
/// name or migration version). That is a build mistake, caught by tests.
pub fn app(db: Database) -> App {
    let modules = modules();
    modules
        .validate()
        .unwrap_or_else(|e| panic!("invalid module list: {e}"));
    let migrations = modules
        .migrations()
        .unwrap_or_else(|e| panic!("invalid module list: {e}"));
    let app = App::new()
        .wrap(SecurityHeaders::default())
        .database(db.migrations(migrations))
        .plugin(Health::new().check("database", DbCheck::new()))
        .openapi(OpenApi::new("SOAT", env!("CARGO_PKG_VERSION")));
    modules.install(app)
}
