//! Creating tenants (shops).
//!
//! Not yet behind authentication: platform-admin auth is the next step before
//! this runs anywhere public.

use soat_core::TenantId;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

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

/// Create a tenant. A taken id is a 409.
#[post("/api/tenants", tag = "tenants", status = 201)]
async fn create_tenant(
    db: Db,
    Valid(Json(t)): Valid<Json<NewTenant>>,
) -> Result<(StatusCode, Json<NewTenant>)> {
    let id = TenantId::parse(&t.id)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_tenant"))?;
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(id.as_str())
        .bind(&t.name)
        .execute(&*db)
        .await?;
    Ok((StatusCode::CREATED, Json(t)))
}
