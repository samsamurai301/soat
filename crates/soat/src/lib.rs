//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! This crate holds no domain code. Each domain is a `soat-*` crate exporting
//! a `Module`; [`modules`] lists the ones this build includes, in dependency
//! order, and [`app`] wires them into one Zapline app.

pub use soat_auth::Auth;
use soat_catalog::Catalog;
use soat_events::Events;
use soat_module::Modules;
use soat_tenancy::Tenancy;
use zapline::db::{Database, DbCheck};
use zapline::middleware::SecurityHeaders;
use zapline::openapi::OpenApi;
use zapline::prelude::*;

/// Every module in this build. A module must come after the ones it needs.
pub fn modules(auth: Auth) -> Modules {
    Modules::new()
        .with(auth)
        .with(Tenancy)
        .with(Events::new())
        .with(Catalog)
}

/// Builds the application on top of `db` (Postgres). `main` passes the real
/// database and `Auth::from_env()`; tests pass a fresh schema each and a
/// fixed setup token (see `tests/common`).
///
/// # Panics
///
/// When the module list is inconsistent (a missing dependency, a duplicate
/// name or migration version). That is a build mistake, caught by tests.
pub fn app(db: Database, auth: Auth) -> App {
    let app = App::new()
        .wrap(SecurityHeaders::default())
        .plugin(Health::new().check("database", DbCheck::new()))
        .openapi(OpenApi::new("SOAT", env!("CARGO_PKG_VERSION")));
    modules(auth).build(app, db)
}
