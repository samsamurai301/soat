//! Resolves the tenant a request acts for.
//!
//! For now the tenant comes from the `x-soat-tenant` header. Later it will come
//! from the request's domain or API key, behind the same `Tenant` extractor, so
//! handlers do not change.

use soat_core::TenantId;
use zapline::db::sqlx::{Postgres, Transaction};
use zapline::db::{sqlx, Db};
use zapline::extract::FromRequestParts;
use zapline::prelude::*;

/// The header that names the tenant.
pub const HEADER: &str = "x-soat-tenant";

/// The Postgres setting row-level security policies read.
pub const SETTING: &str = "soat.tenant";

/// A transaction in which row-level security sees one tenant. Run every
/// query on tenant-owned tables on it (`&mut *tx`), then `tx.commit()`.
pub type TenantTx = Transaction<'static, Postgres>;

/// The tenant named by the request. Call [`Tenant::begin`] before touching
/// its data.
pub struct Tenant(pub TenantId);

impl FromRequestParts for Tenant {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let raw = req
            .headers()
            .get(HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                Error::bad_request("missing x-soat-tenant header").with_code("missing_tenant")
            })?;
        let id = TenantId::parse(raw)
            .map_err(|e| Error::bad_request(e.to_string()).with_code("invalid_tenant"))?;
        Ok(Tenant(id))
    }
}

impl Tenant {
    /// Checks the tenant exists, so an unknown tenant is a clear 404 rather
    /// than an empty list or a foreign-key error.
    pub async fn verify(&self, db: &Db) -> Result<()> {
        let found: Option<(String,)> = sqlx::query_as("SELECT id FROM tenants WHERE id = $1")
            .bind(self.0.as_str())
            .fetch_optional(&**db)
            .await?;
        match found {
            Some(_) => Ok(()),
            None => Err(Error::not_found().with_code("unknown_tenant")),
        }
    }

    /// Verifies the tenant and opens a transaction scoped to it: the
    /// `soat.tenant` setting is set for this transaction only, so the
    /// row-level security policy on every tenant-owned table lets through
    /// this tenant's rows and nothing else, even if a query forgets its
    /// `WHERE tenant_id`.
    pub async fn begin(&self, db: &Db) -> Result<TenantTx> {
        self.verify(db).await?;
        let mut tx = db.begin().await?;
        sqlx::query("SELECT set_config($1, $2, true)")
            .bind(SETTING)
            .bind(self.0.as_str())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    pub fn id(&self) -> &str {
        self.0.as_str()
    }
}
