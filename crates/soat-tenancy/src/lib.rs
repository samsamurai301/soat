//! Tenancy: every shop on a SOAT install is one tenant.
//!
//! This module owns the `tenants` table and `POST /api/tenants`, and exports
//! the [`Tenant`] extractor that every tenant-owned module uses to scope its
//! queries.

mod extract;
pub mod routes;

pub use extract::{Tenant, HEADER};

use soat_module::Module;
use zapline::embed::Dir;
use zapline::prelude::App;

/// The tenancy module. List it before any module that owns tenant data.
pub struct Tenancy;

impl Module for Tenancy {
    fn name(&self) -> &'static str {
        "tenancy"
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn mount(&self, app: App) -> App {
        app.plugin(routes::mount)
    }
}
