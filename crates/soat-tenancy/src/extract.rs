//! Resolves the shop a request acts for, and who acts.
//!
//! For now the shop comes from the `x-soat-tenant` header and the person from
//! a bearer token. Later the shop will also come from the request's domain or
//! an API key (A7, A11), behind the same `Tenant` extractor, so handlers do
//! not change.

use soat_auth::Staff;
use soat_core::TenantId;
use zapline::db::sqlx::{PgConnection, Postgres, Transaction};
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

/// The shop named by the request and the signed-in user acting for it.
/// Call [`Tenant::begin`] before touching its data: that is where
/// membership is checked.
pub struct Tenant {
    pub id: TenantId,
    pub staff: Staff,
}

impl FromRequestParts for Tenant {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let raw = req
            .headers()
            .get(HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                Error::bad_request("missing x-soat-tenant header").with_code("missing_tenant")
            })?
            .to_owned();
        let id = TenantId::parse(&raw)
            .map_err(|e| Error::bad_request(e.to_string()).with_code("invalid_tenant"))?;
        let staff = Staff::from_request_parts(req).await?;
        Ok(Tenant { id, staff })
    }
}

/// What a member may do in a shop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Staff,
}

impl Tenant {
    /// Opens a transaction scoped to this shop: the `soat.tenant` setting is
    /// set for this transaction only, so the row-level security policy on
    /// every tenant-owned table lets through this shop's rows and nothing
    /// else, even if a query forgets its `WHERE tenant_id`.
    ///
    /// A shop that does not exist, or that the user is not a member of, is
    /// the same 404 (`unknown_tenant`), so shop names do not leak. Platform
    /// admins may act in any shop.
    pub async fn begin(&self, db: &Db) -> Result<TenantTx> {
        Ok(self.begin_with_role(db).await?.0)
    }

    /// [`begin`](Self::begin), then 403 (`owner_only`) unless the user owns
    /// the shop or is a platform admin.
    pub async fn begin_owner(&self, db: &Db) -> Result<TenantTx> {
        let (tx, role) = self.begin_with_role(db).await?;
        if role == Role::Owner {
            Ok(tx)
        } else {
            Err(Error::forbidden().with_code("owner_only"))
        }
    }

    async fn begin_with_role(&self, db: &Db) -> Result<(TenantTx, Role)> {
        let mut tx = db.begin().await?;
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM tenants WHERE id = $1")
            .bind(self.id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(unknown());
        }
        enter(&mut tx, &self.id).await?;
        if self.staff.is_platform_admin {
            return Ok((tx, Role::Owner));
        }
        let role: Option<(String,)> =
            sqlx::query_as("SELECT role FROM memberships WHERE user_id = $1")
                .bind(self.staff.user_id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        match role.as_ref().map(|(r,)| r.as_str()) {
            Some("owner") => Ok((tx, Role::Owner)),
            Some(_) => Ok((tx, Role::Staff)),
            None => Err(unknown()),
        }
    }

    pub fn id(&self) -> &str {
        self.id.as_str()
    }
}

/// Points row-level security at `tenant` for the rest of the transaction.
pub async fn enter(conn: &mut PgConnection, tenant: &TenantId) -> Result<()> {
    sqlx::query("SELECT set_config($1, $2, true)")
        .bind(SETTING)
        .bind(tenant.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn unknown() -> Error {
    Error::not_found().with_code("unknown_tenant")
}
