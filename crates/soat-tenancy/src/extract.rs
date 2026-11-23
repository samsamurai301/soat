//! Resolves the shop a request acts for, and who acts.
//!
//! The shop comes, in order, from an API key (`soat_sk_<shop>_…`, which
//! names it), the `x-soat-tenant` header, or the request's `Host` when it is
//! one of the shop's domains. The person comes from a bearer token. Handlers
//! only see [`Tenant`].

use std::collections::HashSet;

use soat_auth::Staff;
use soat_core::{Domain, Permission, TenantId};
use soat_module::Permissions;
use zapline::auth::password::hash_token;
use zapline::auth::BearerToken;
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

/// The prefix of every SOAT API key: `soat_sk_<shop>_<secret>`.
pub const API_KEY_PREFIX: &str = "soat_sk_";

/// The shop a request acts for and who acts: a signed-in user or an API
/// key. Call [`Tenant::begin`] before touching its data: that is where
/// membership or the key is checked.
pub struct Tenant {
    pub id: TenantId,
    pub actor: Actor,
    permissions: Permissions,
    /// `user_...` or `key_...`, known once `begin` checked the caller.
    actor_id: std::sync::OnceLock<String>,
}

/// Who acts for the shop.
#[derive(Debug, Clone)]
pub enum Actor {
    /// A signed-in user, checked against the shop's memberships.
    Staff(Staff),
    /// An API key of the shop (scripts, apps, Claude). Only its SHA-256 is
    /// known until [`Tenant::begin`] looks it up.
    ApiKey { token_hash: String },
}

impl FromRequestParts for Tenant {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let header = req
            .headers()
            .get(HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|raw| {
                TenantId::parse(raw)
                    .map_err(|e| Error::bad_request(e.to_string()).with_code("invalid_tenant"))
            })
            .transpose()?;
        let bearer = BearerToken::from_request_parts(req).await.ok();
        let api_key = bearer
            .as_ref()
            .and_then(|b| b.token().strip_prefix(API_KEY_PREFIX));
        let (id, actor) = if let Some(rest) = api_key {
            // The key names its shop; a header, if sent, must agree.
            let shop = rest
                .split_once('_')
                .and_then(|(shop, _)| TenantId::parse(shop).ok())
                .ok_or_else(|| Error::unauthorized().with_code("invalid_api_key"))?;
            if header.as_ref().is_some_and(|h| *h != shop) {
                return Err(unknown());
            }
            let token_hash = hash_token(bearer.as_ref().map(|b| b.token()).unwrap_or_default());
            (shop, Actor::ApiKey { token_hash })
        } else {
            let id = match header {
                Some(id) => id,
                None => from_host(req).await?.ok_or_else(|| {
                    Error::bad_request("missing x-soat-tenant header").with_code("missing_tenant")
                })?,
            };
            (id, Actor::Staff(Staff::from_request_parts(req).await?))
        };
        let permissions = State::<Permissions>::from_request_parts(req)
            .await
            .map(|State(p)| p)
            .unwrap_or_default();
        Ok(Tenant {
            id,
            actor,
            permissions,
            actor_id: std::sync::OnceLock::new(),
        })
    }
}

/// Who a library function acts for: a shop and, when known, who acts in
/// it. [`Tenant`] (staff and API keys) and [`Shop`] (public requests)
/// both are one, so catalog, pricing and inventory helpers serve the admin
/// API and the storefront alike.
pub trait Scope: Sync {
    /// The shop's id.
    fn tenant_id(&self) -> &str;

    /// Who acts, as recorded on events: `user_...`, `key_...`, `cus_...`,
    /// or `None`.
    fn actor_id(&self) -> Option<&str>;

    /// A new transaction that sees only this shop. It checks no
    /// permission: callers have checked one on the request already.
    fn begin(&self, db: &Db) -> impl std::future::Future<Output = Result<TenantTx>> + Send {
        async move {
            let id = TenantId::parse(self.tenant_id()).map_err(|_| unknown())?;
            let mut tx = db.begin().await?;
            enter(&mut tx, &id).await?;
            Ok(tx)
        }
    }

    /// Publishes a domain event in the transaction of the change.
    fn publish(
        &self,
        conn: &mut PgConnection,
        kind: &str,
        payload: &(impl serde::Serialize + Sync),
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            soat_events::publish(conn, self.tenant_id(), self.actor_id(), kind, payload).await?;
            Ok(())
        }
    }
}

impl Scope for Tenant {
    fn tenant_id(&self) -> &str {
        self.id()
    }

    fn actor_id(&self) -> Option<&str> {
        Tenant::actor_id(self)
    }
}

impl Scope for Shop {
    fn tenant_id(&self) -> &str {
        self.id()
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// The shop an event belongs to, for an event subscriber: the
/// system acts.
pub struct EventShop(pub String);

impl Scope for EventShop {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

impl EventShop {
    /// A transaction on `pool` that sees only this shop.
    ///
    /// # Errors
    ///
    /// A malformed shop id; database errors.
    pub async fn begin_on(&self, pool: &zapline::db::sqlx::PgPool) -> Result<TenantTx> {
        let id = TenantId::parse(&self.0).map_err(|_| unknown())?;
        let mut tx = pool.begin().await?;
        enter(&mut tx, &id).await?;
        Ok(tx)
    }
}

/// A shop and who acts in it on a public request, e.g. a signed-in
/// customer (`cus_...`).
pub struct Acting<'a> {
    pub shop: &'a Shop,
    pub actor: Option<String>,
}

impl Scope for Acting<'_> {
    fn tenant_id(&self) -> &str {
        self.shop.id()
    }

    fn actor_id(&self) -> Option<&str> {
        self.actor.as_deref()
    }
}

/// The shop a public request (storefront, customer accounts) is for, with
/// no staff or API key involved: from the `x-soat-tenant` header, else the
/// `Host` among the shop's domains. Its data is reached with
/// [`Shop::begin`], which applies row-level security like [`Tenant`].
#[derive(Debug, Clone)]
pub struct Shop {
    pub id: TenantId,
}

impl FromRequestParts for Shop {
    async fn from_request_parts(req: &mut Request) -> Result<Self> {
        let header = req
            .headers()
            .get(HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|raw| TenantId::parse(raw).map_err(|_| unknown()))
            .transpose()?;
        let id = match header {
            Some(id) => id,
            None => from_host(req).await?.ok_or_else(unknown)?,
        };
        Ok(Shop { id })
    }
}

impl Shop {
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    /// A transaction that sees only this shop; 404 (`unknown_tenant`) when
    /// it does not exist.
    pub async fn begin(&self, db: &Db) -> Result<TenantTx> {
        let mut tx = db.begin().await?;
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM tenants WHERE id = $1")
            .bind(self.id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(unknown());
        }
        enter(&mut tx, &self.id).await?;
        Ok(tx)
    }

    /// Publishes a domain event of this shop caused by `actor` (e.g. a
    /// customer id), in the transaction of the change.
    pub async fn publish(
        &self,
        conn: &mut PgConnection,
        actor: Option<&str>,
        kind: &str,
        payload: &impl serde::Serialize,
    ) -> Result<()> {
        soat_events::publish(conn, self.id(), actor, kind, payload).await?;
        Ok(())
    }
}

/// The shop whose domain the request's `Host` is, if any.
async fn from_host(req: &mut Request) -> Result<Option<TenantId>> {
    let Some(domain) = req
        .headers()
        .get(zapline::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| Domain::from_host(h).ok())
    else {
        return Ok(None);
    };
    let db = Db::from_request_parts(req).await?;
    crate::domains::tenant_for_domain(&db, &domain).await
}

/// The built-in role with every permission.
pub const OWNER: &str = "owner";
/// The built-in role with every permission a module marks `for_staff`.
pub const STAFF: &str = "staff";

/// What the caller may do in this shop.
#[derive(Debug, Clone)]
pub struct Access {
    /// `owner`, `staff` or a custom role name. Platform admins count as owner.
    pub role: String,
    granted: Granted,
}

#[derive(Debug, Clone)]
enum Granted {
    All,
    Only(HashSet<String>),
}

impl Access {
    pub fn is_owner(&self) -> bool {
        matches!(self.granted, Granted::All)
    }

    pub fn allows(&self, permission: &str) -> bool {
        match &self.granted {
            Granted::All => true,
            Granted::Only(set) => set.contains(permission),
        }
    }

    /// 403 (`missing_permission`) unless this access allows `permission`.
    pub fn require(&self, permission: &Permission) -> Result<()> {
        if self.allows(permission.name) {
            Ok(())
        } else {
            Err(Error::new(
                StatusCode::FORBIDDEN,
                format!("this needs the {} permission", permission.name),
            )
            .with_code("missing_permission"))
        }
    }
}

impl Tenant {
    /// Opens a transaction scoped to this shop for any member: the
    /// `soat.tenant` setting is set for this transaction only, so the
    /// row-level security policy on every tenant-owned table lets through
    /// this shop's rows and nothing else, even if a query forgets its
    /// `WHERE tenant_id`.
    ///
    /// A shop that does not exist, or that the user is not a member of, is
    /// the same 404 (`unknown_tenant`), so shop names do not leak. Platform
    /// admins may act in any shop, as owners.
    pub async fn begin(&self, db: &Db) -> Result<TenantTx> {
        Ok(self.begin_with_access(db).await?.0)
    }

    /// [`begin`](Self::begin), then 403 (`missing_permission`) unless the
    /// caller's role grants `permission`.
    pub async fn begin_for(&self, db: &Db, permission: &Permission) -> Result<TenantTx> {
        let (tx, access) = self.begin_with_access(db).await?;
        access.require(permission)?;
        Ok(tx)
    }

    /// [`begin`](Self::begin), also returning what the caller may do.
    pub async fn begin_with_access(&self, db: &Db) -> Result<(TenantTx, Access)> {
        let mut tx = db.begin().await?;
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM tenants WHERE id = $1")
            .bind(self.id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(unknown());
        }
        enter(&mut tx, &self.id).await?;
        let staff = match &self.actor {
            Actor::Staff(staff) => staff,
            Actor::ApiKey { token_hash } => {
                let key: Option<(String, String, Vec<String>)> = sqlx::query_as(
                    "SELECT id, name, permissions FROM api_keys \
                     WHERE token_hash = $1 AND revoked_at IS NULL",
                )
                .bind(token_hash)
                .fetch_optional(&mut *tx)
                .await?;
                let Some((key_id, name, permissions)) = key else {
                    return Err(Error::unauthorized().with_code("invalid_api_key"));
                };
                let _ = self.actor_id.set(key_id);
                let access = Access {
                    role: format!("api_key:{name}"),
                    granted: Granted::Only(permissions.into_iter().collect()),
                };
                return Ok((tx, access));
            }
        };
        let _ = self.actor_id.set(staff.user_id.to_string());
        if staff.is_platform_admin {
            return Ok((tx, owner()));
        }
        let role: Option<(String,)> =
            sqlx::query_as("SELECT role FROM memberships WHERE user_id = $1")
                .bind(staff.user_id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((role,)) = role else {
            return Err(unknown());
        };
        let access = self.resolve(&mut tx, role).await?;
        Ok((tx, access))
    }

    /// The access a role name grants in this shop. Call inside the shop's
    /// transaction.
    pub async fn resolve(&self, conn: &mut PgConnection, role: String) -> Result<Access> {
        let granted = match role.as_str() {
            OWNER => return Ok(owner()),
            STAFF => self
                .permissions
                .staff_defaults()
                .map(str::to_owned)
                .collect(),
            _ => {
                let row: Option<(Vec<String>,)> =
                    sqlx::query_as("SELECT permissions FROM roles WHERE name = $1")
                        .bind(&role)
                        .fetch_optional(&mut *conn)
                        .await?;
                row.map(|(p,)| p.into_iter().collect()).unwrap_or_default()
            }
        };
        Ok(Access {
            role,
            granted: Granted::Only(granted),
        })
    }

    /// Every permission the app's modules declare.
    pub fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    /// Who acts, as recorded on events: `user_...` or `key_...`. `None`
    /// before [`begin`](Self::begin) has checked the caller.
    pub fn actor_id(&self) -> Option<&str> {
        self.actor_id.get().map(String::as_str)
    }

    /// Publishes a domain event for this shop, recording who caused it. Call
    /// it in the transaction of the change.
    pub async fn publish(
        &self,
        conn: &mut PgConnection,
        kind: &str,
        payload: &impl serde::Serialize,
    ) -> Result<()> {
        soat_events::publish(conn, self.id(), self.actor_id(), kind, payload).await?;
        Ok(())
    }

    /// The signed-in user, or `None` for an API key.
    pub fn staff(&self) -> Option<&Staff> {
        match &self.actor {
            Actor::Staff(staff) => Some(staff),
            Actor::ApiKey { .. } => None,
        }
    }
}

fn owner() -> Access {
    Access {
        role: OWNER.to_owned(),
        granted: Granted::All,
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
