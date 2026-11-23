//! Tenancy: every shop on a SOAT install is one tenant.
//!
//! This module owns the `tenants` and `memberships` tables, creating shops
//! (platform admins only) and managing their staff, and exports the
//! [`Tenant`] extractor that every tenant-owned module uses to scope its
//! queries.
//!
//! Isolation is enforced twice: queries filter on `tenant_id`, and Postgres
//! row-level security on every tenant-owned table only shows rows whose
//! `tenant_id` matches the `soat.tenant` setting, which [`Tenant::begin`]
//! sets per transaction. A tenant-owned table's migration enables it:
//!
//! ```sql
//! ALTER TABLE things ENABLE ROW LEVEL SECURITY;
//! ALTER TABLE things FORCE ROW LEVEL SECURITY;
//! CREATE POLICY tenant_isolation ON things
//!     USING (tenant_id = current_setting('soat.tenant', true))
//!     WITH CHECK (tenant_id = current_setting('soat.tenant', true));
//! ```

mod extract;
mod list;
pub mod routes;

pub use extract::{enter, Role, Tenant, TenantTx, HEADER, SETTING};
pub use list::TenantListing;

use soat_module::Module;
use zapline::embed::Dir;
use zapline::prelude::App;

/// The tenancy module. List it before any module that owns tenant data.
pub struct Tenancy;

impl Module for Tenancy {
    fn name(&self) -> &'static str {
        "tenancy"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn mount(&self, app: App) -> App {
        app.plugin(routes::mount)
    }
}
