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

pub mod api_keys;
pub mod domains;
pub mod event_log;
mod extract;
mod list;
pub mod routes;
pub mod shop;
pub mod staff;

pub use extract::{
    enter, Access, Acting, Actor, EventShop, Scope, Shop, Tenant, TenantTx, API_KEY_PREFIX, HEADER,
    OWNER, SETTING, STAFF,
};
pub use list::TenantListing;

use soat_core::Permission;
use soat_module::Module;
use zapline::embed::Dir;
use zapline::prelude::App;

/// See the shop's staff and roles.
pub const STAFF_READ: Permission =
    Permission::new("staff:read", "See the shop's staff and roles").for_staff();
/// Add staff and change their roles, up to the caller's own permissions.
pub const STAFF_MANAGE: Permission = Permission::new(
    "staff:manage",
    "Add staff and change their roles (never beyond your own permissions)",
);

/// The tenancy module. List it before any module that owns tenant data.
#[derive(Default)]
pub struct Tenancy {
    proof: domains::DomainProof,
}

impl Tenancy {
    /// Domains serve a shop as soon as it adds them: for installs that
    /// serve one shop, or whose operator adds the domains.
    pub fn new() -> Self {
        Self::default()
    }

    /// A shop must show it owns a domain before it serves the shop: a DNS
    /// TXT record, looked up with `lookup` (see [`domains`]).
    pub fn with_domain_proof(mut self, lookup: std::sync::Arc<dyn domains::TxtLookup>) -> Self {
        self.proof = domains::DomainProof::Dns(lookup);
        self
    }
}

impl Module for Tenancy {
    fn name(&self) -> &'static str {
        "tenancy"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["auth", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[
            STAFF_READ,
            STAFF_MANAGE,
            api_keys::API_KEYS_MANAGE,
            domains::DOMAINS_MANAGE,
            event_log::EVENTS_READ,
            shop::SHOP_READ,
            shop::SHOP_MANAGE,
        ]
    }

    fn mount(&self, app: App) -> App {
        app.plugin(routes::mount)
            .plugin(staff::mount)
            .plugin(api_keys::mount)
            .state(self.proof.clone())
            .plugin(domains::mount)
            .plugin(event_log::mount)
            .plugin(shop::mount)
    }
}
