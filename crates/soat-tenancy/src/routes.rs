//! Creating shops.

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

pub fn mount(app: App) -> App {
    app.mount(create_tenant)
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
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(t)))
}
