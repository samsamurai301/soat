//! SOAT: a commerce engine that runs as one Rust binary, built on zapline.
//!
//! Modules live under `routes/`, one file per module. Every tenant-owned query
//! is scoped by the request's tenant (`tenant::Tenant`).

pub mod routes;
pub mod tenant;

use zapline::db::{Database, DbCheck};
use zapline::middleware::SecurityHeaders;
use zapline::openapi::OpenApi;
use zapline::prelude::*;

/// Builds the application on top of `db`. `main` passes the real database;
/// tests pass `Database::sqlite_memory()`.
pub fn app(db: Database) -> App {
    App::new()
        .wrap(SecurityHeaders::default())
        .database(db.migrations(zapline::embed!("migrations")))
        .plugin(Health::new().check("database", DbCheck::new()))
        .openapi(OpenApi::new("SOAT", env!("CARGO_PKG_VERSION")))
        .plugin(routes::install)
}
