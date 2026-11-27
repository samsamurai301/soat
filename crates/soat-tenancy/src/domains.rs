//! The domains a shop is served on. A request without an API key or an
//! `x-soat-tenant` header acts for the shop its `Host` belongs to.
//!
//! On an install that serves many shops, a shop proves it owns a domain
//! before the domain serves it: adding one gives a token for a DNS TXT
//! record at `_soat-challenge.<domain>`, and `POST
//! /api/domains/{domain}/verify` looks it up. Any number of shops may
//! claim a domain; the first to prove it gets it, so nobody can hold a
//! domain they do not own.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use soat_core::{Domain, Permission, TenantId};
use zapline::auth::password::new_token;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::Tenant;

/// Add and remove the shop's domains.
pub const DOMAINS_MANAGE: Permission =
    Permission::new("domains:manage", "Add and remove the shop's domains");

/// Where the TXT record goes: `_soat-challenge.<domain>`.
pub const CHALLENGE_PREFIX: &str = "_soat-challenge.";

/// What a TXT lookup returns.
pub type TxtFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<Vec<String>, String>> + Send + 'a>>;

/// Looks up a name's TXT records: each record's strings joined. An error
/// says why, for staff to read; a name with no records is an empty list.
pub trait TxtLookup: Send + Sync {
    fn txt<'a>(&'a self, name: &'a str) -> TxtFuture<'a>;
}

/// How a shop shows it owns a domain: an app state.
#[derive(Clone, Default)]
pub enum DomainProof {
    /// Domains serve the shop as soon as it adds them.
    #[default]
    None,
    /// A DNS TXT record, looked up with this.
    Dns(Arc<dyn TxtLookup>),
}

/// A domain the shop is served on, or has asked for.
#[model]
pub struct ShopDomain {
    /// e.g. `shop.example.com`; stored lowercase, without a port.
    pub domain: String,
    /// It serves the shop. Until then, the TXT record below proves it.
    pub verified: bool,
    /// Where to add the TXT record, e.g. `_soat-challenge.shop.example.com`.
    pub txt_name: Option<String>,
    /// The TXT record's value, e.g. `soat-verify=…`.
    pub txt_value: Option<String>,
}

/// Body of `POST /api/domains`.
#[model]
pub struct NewDomain {
    /// e.g. `shop.example.com`.
    #[validate(length(min = 3, max = 253))]
    pub domain: String,
}

pub(crate) fn mount(app: App) -> App {
    app.mount(list_domains)
        .mount(add_domain)
        .mount(verify_domain)
        .mount(remove_domain)
}

fn verified(domain: String) -> ShopDomain {
    ShopDomain {
        domain,
        verified: true,
        txt_name: None,
        txt_value: None,
    }
}

fn pending(domain: String, token: &str) -> ShopDomain {
    ShopDomain {
        txt_name: Some(format!("{CHALLENGE_PREFIX}{domain}")),
        txt_value: Some(format!("soat-verify={token}")),
        domain,
        verified: false,
    }
}

/// The shop's domains, then the ones it still has to verify
/// (`domains:manage`).
#[get("/api/domains", tag = "domains")]
async fn list_domains(db: Db, tenant: Tenant) -> Result<Json<Vec<ShopDomain>>> {
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let live: Vec<(String,)> =
        sqlx::query_as("SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY domain")
            .bind(tenant.id())
            .fetch_all(&mut *tx)
            .await?;
    let claims: Vec<(String, String)> = sqlx::query_as(
        "SELECT domain, token FROM domain_claims WHERE tenant_id = $1 ORDER BY domain",
    )
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(
        live.into_iter()
            .map(|(d,)| verified(d))
            .chain(claims.into_iter().map(|(d, t)| pending(d, &t)))
            .collect(),
    ))
}

/// Serve the shop on a domain (`domains:manage`). Where the install asks
/// shops to prove they own their domains, the answer has `verified:
/// false` and the TXT record to add before `POST
/// /api/domains/{domain}/verify`; otherwise it serves the shop at once.
/// A domain another shop has is a 409 `domain_taken`. Point its DNS at
/// this install too. Publishes `domain.added` once it serves the shop.
#[post("/api/domains", tag = "domains", status = 201)]
async fn add_domain(
    db: Db,
    tenant: Tenant,
    proof: State<DomainProof>,
    Valid(Json(body)): Valid<Json<NewDomain>>,
) -> Result<(StatusCode, Json<ShopDomain>)> {
    let domain = Domain::parse(&body.domain)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_domain"))?;
    if tenant_for_domain(&db, &domain).await?.is_some() {
        return Err(Error::conflict("this domain is already in use").with_code("domain_taken"));
    }
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let out = match &proof.0 {
        DomainProof::None => {
            claim(&mut tx, &tenant, &domain).await?;
            verified(domain.to_string())
        }
        DomainProof::Dns(_) => {
            let existing: Option<(String,)> = sqlx::query_as(
                "SELECT token FROM domain_claims WHERE tenant_id = $1 AND domain = $2",
            )
            .bind(tenant.id())
            .bind(domain.as_str())
            .fetch_optional(&mut *tx)
            .await?;
            let token = match existing {
                Some((t,)) => t,
                None => {
                    let t = new_token()?;
                    sqlx::query(
                        "INSERT INTO domain_claims (tenant_id, domain, token) VALUES ($1, $2, $3)",
                    )
                    .bind(tenant.id())
                    .bind(domain.as_str())
                    .bind(&t)
                    .execute(&mut *tx)
                    .await?;
                    t
                }
            };
            pending(domain.to_string(), &token)
        }
    };
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// The domain serves the shop from now on; publishes `domain.added`.
async fn claim(tx: &mut crate::TenantTx, tenant: &Tenant, domain: &Domain) -> Result<()> {
    let added = sqlx::query(
        "INSERT INTO tenant_domains (domain, tenant_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(domain.as_str())
    .bind(tenant.id())
    .execute(&mut **tx)
    .await?;
    if added.rows_affected() == 0 {
        return Err(Error::conflict("this domain is already in use").with_code("domain_taken"));
    }
    sqlx::query("DELETE FROM domain_claims WHERE tenant_id = $1 AND domain = $2")
        .bind(tenant.id())
        .bind(domain.as_str())
        .execute(&mut **tx)
        .await?;
    tenant
        .publish(&mut *tx, "domain.added", &verified(domain.to_string()))
        .await?;
    Ok(())
}

/// Check the TXT record of a domain the shop asked for
/// (`domains:manage`): when `_soat-challenge.<domain>` has the value it
/// was given, the domain serves the shop (publishes `domain.added`). 422
/// `not_verified` says what was found instead; 409 `domain_taken` when
/// another shop proved it first; 404 for a domain the shop did not ask
/// for.
#[post("/api/domains/{domain}/verify", tag = "domains")]
async fn verify_domain(
    db: Db,
    tenant: Tenant,
    proof: State<DomainProof>,
    Path(domain): Path<String>,
) -> Result<Json<ShopDomain>> {
    let domain = Domain::parse(&domain).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let token: Option<(String,)> =
        sqlx::query_as("SELECT token FROM domain_claims WHERE tenant_id = $1 AND domain = $2")
            .bind(tenant.id())
            .bind(domain.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    let Some((token,)) = token else {
        return Err(Error::not_found());
    };
    let expected = format!("soat-verify={token}");
    let name = format!("{CHALLENGE_PREFIX}{domain}");
    if let DomainProof::Dns(lookup) = &proof.0 {
        let found = lookup.txt(&name).await.map_err(|e| {
            Error::unprocessable(format!("{name} could not be looked up: {e}"))
                .with_code("not_verified")
        })?;
        if !found.iter().any(|v| v.trim() == expected) {
            let saw = if found.is_empty() {
                "no TXT record".to_owned()
            } else {
                found.join(", ")
            };
            return Err(Error::unprocessable(format!(
                "{name} should have a TXT record {expected}; it has {saw}. DNS changes can take a while to show."
            ))
            .with_code("not_verified"));
        }
    }
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    claim(&mut tx, &tenant, &domain).await?;
    tx.commit().await?;
    Ok(Json(verified(domain.to_string())))
}

/// Stop serving the shop on a domain, or drop a claim to one
/// (`domains:manage`). Publishes `domain.removed` for one that served it.
#[delete("/api/domains/{domain}", tag = "domains", status = 204)]
async fn remove_domain(db: Db, tenant: Tenant, Path(domain): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let domain = Domain::parse(&domain).map_err(|_| Error::not_found())?;
    let claimed = sqlx::query("DELETE FROM domain_claims WHERE domain = $1 AND tenant_id = $2")
        .bind(domain.as_str())
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let removed = sqlx::query("DELETE FROM tenant_domains WHERE domain = $1 AND tenant_id = $2")
        .bind(domain.as_str())
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed + claimed == 0 {
        return Err(Error::not_found());
    }
    if removed > 0 {
        tenant
            .publish(&mut tx, "domain.removed", &verified(domain.to_string()))
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The shop a domain belongs to, if any. Reads across shops, so it runs in
/// its own read-only transaction with `soat.system` on.
pub async fn tenant_for_domain(db: &Db, domain: &Domain) -> Result<Option<TenantId>> {
    let mut tx = db.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('soat.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tenant_id FROM tenant_domains WHERE domain = $1")
            .bind(domain.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(row.and_then(|(id,)| TenantId::parse(&id).ok()))
}
