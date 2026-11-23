//! Creating shops, and the shops a user works for.

use soat_auth::Staff;
use soat_core::TenantId;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::extract::enter;

/// A shop on this install.
#[model]
pub struct NewTenant {
    /// Slug used in URLs and the `x-soat-tenant` header, e.g. `acme`.
    #[validate(length(min = 2, max = 63))]
    pub id: String,
    #[validate(length(min = 1, max = 200))]
    pub name: String,
}

/// A shop the signed-in user works for.
#[model]
#[derive(sqlx::FromRow)]
pub struct MyTenant {
    pub id: String,
    pub name: String,
    /// `owner` or `staff`; null for a platform admin who is no member.
    pub role: Option<String>,
}

pub fn mount(app: App) -> App {
    app.mount((create_tenant, my_tenants))
}

/// The shops the signed-in user works for, by name, with their role.
/// Platform admins see every shop.
#[get("/api/tenants", tag = "tenants")]
async fn my_tenants(db: Db, staff: Staff) -> Result<Json<Vec<MyTenant>>> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('soat.user', $1, true)")
        .bind(staff.user_id.as_str())
        .execute(&mut *tx)
        .await?;
    let shops = sqlx::query_as::<_, MyTenant>(
        "SELECT t.id, t.name, m.role FROM tenants t \
         LEFT JOIN memberships m ON m.tenant_id = t.id AND m.user_id = $1 \
         WHERE m.user_id IS NOT NULL OR $2 ORDER BY t.name, t.id",
    )
    .bind(staff.user_id.as_str())
    .bind(staff.is_platform_admin)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(shops))
}

/// Create a shop. Platform admins only; the creator becomes its owner.
/// A taken id is a 409.
#[post("/api/tenants", tag = "tenants", status = 201)]
async fn create_tenant(
    db: Db,
    staff: Staff,
    Valid(Json(t)): Valid<Json<NewTenant>>,
) -> Result<(StatusCode, Json<NewTenant>)> {
    staff.require_platform_admin()?;
    let id = TenantId::parse(&t.id)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_tenant"))?;
    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(id.as_str())
        .bind(&t.name)
        .execute(&mut *tx)
        .await?;
    enter(&mut tx, &id).await?;
    sqlx::query("INSERT INTO memberships (tenant_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(id.as_str())
        .bind(staff.user_id.as_str())
        .execute(&mut *tx)
        .await?;
    soat_events::publish(
        &mut tx,
        id.as_str(),
        Some(staff.user_id.as_str()),
        "tenant.created",
        &t,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(t)))
}
