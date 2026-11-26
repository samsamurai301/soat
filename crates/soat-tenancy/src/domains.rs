//! The domains a shop is served on. A request without an API key or an
//! `x-soat-tenant` header acts for the shop its `Host` belongs to.

use soat_core::{Domain, Permission, TenantId};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::Tenant;

/// Add and remove the shop's domains.
pub const DOMAINS_MANAGE: Permission =
    Permission::new("domains:manage", "Add and remove the shop's domains");

/// A domain the shop is served on.
#[model]
#[derive(sqlx::FromRow)]
pub struct ShopDomain {
    /// e.g. `shop.example.com`; stored lowercase, without a port.
    #[validate(length(min = 3, max = 253))]
    pub domain: String,
}

pub(crate) fn mount(app: App) -> App {
    app.mount(list_domains)
        .mount(add_domain)
        .mount(remove_domain)
}

/// The shop's domains (`domains:manage`).
#[get("/api/domains", tag = "domains")]
async fn list_domains(db: Db, tenant: Tenant) -> Result<Json<Vec<ShopDomain>>> {
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let domains = sqlx::query_as::<_, ShopDomain>(
        "SELECT domain FROM tenant_domains WHERE tenant_id = $1 ORDER BY domain",
    )
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(domains))
}

/// Serve the shop on a domain (`domains:manage`). A domain another shop
/// uses is a 409. Point its DNS at this install first.
#[post("/api/domains", tag = "domains", status = 201)]
async fn add_domain(
    db: Db,
    tenant: Tenant,
    Valid(Json(body)): Valid<Json<ShopDomain>>,
) -> Result<(StatusCode, Json<ShopDomain>)> {
    let domain = Domain::parse(&body.domain)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_domain"))?;
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    if tenant_for_domain(&db, &domain).await?.is_some() {
        return Err(Error::conflict("this domain is already in use").with_code("domain_taken"));
    }
    sqlx::query("INSERT INTO tenant_domains (domain, tenant_id) VALUES ($1, $2)")
        .bind(domain.as_str())
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    let added = ShopDomain {
        domain: domain.to_string(),
    };
    tenant.publish(&mut tx, "domain.added", &added).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(added)))
}

/// Stop serving the shop on a domain (`domains:manage`).
#[delete("/api/domains/{domain}", tag = "domains", status = 204)]
async fn remove_domain(db: Db, tenant: Tenant, Path(domain): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &DOMAINS_MANAGE).await?;
    let domain = Domain::parse(&domain).map_err(|_| Error::not_found())?;
    let removed = sqlx::query("DELETE FROM tenant_domains WHERE domain = $1 AND tenant_id = $2")
        .bind(domain.as_str())
        .bind(tenant.id())
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    let removed = ShopDomain {
        domain: domain.to_string(),
    };
    tenant.publish(&mut tx, "domain.removed", &removed).await?;
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
