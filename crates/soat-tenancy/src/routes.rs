//! Creating shops and managing their staff.

use soat_auth::{create_user, normalize_email, Staff};
use soat_core::TenantId;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::extract::enter;
use crate::Tenant;

/// A shop on this install.
#[model]
pub struct NewTenant {
    /// Slug used in URLs and the `x-soat-tenant` header, e.g. `acme`.
    #[validate(length(min = 2, max = 63))]
    pub id: String,
    #[validate(length(min = 1, max = 200))]
    pub name: String,
}

/// Someone to add to the shop's staff.
#[model]
pub struct NewStaff {
    #[validate(email)]
    pub email: String,
    /// Required when no account exists for this email yet (at least 12
    /// characters); ignored when one does.
    #[validate(length(min = 12, max = 1024))]
    pub password: Option<String>,
    /// `owner` or `staff` (the default).
    pub role: Option<String>,
}

/// A member of a shop's staff.
#[model]
#[derive(sqlx::FromRow)]
pub struct Member {
    pub user_id: String,
    pub email: String,
    pub role: String,
}

pub fn mount(app: App) -> App {
    app.mount((create_tenant, add_staff, list_staff))
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

/// Add someone to the shop's staff. Owners only. Creates their account
/// when the email is new (then `password` is required); a person who is
/// already a member is a 409.
#[post("/api/staff", tag = "tenants", status = 201)]
async fn add_staff(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<NewStaff>>,
) -> Result<(StatusCode, Json<Member>)> {
    let role = match s.role.as_deref() {
        None | Some("staff") => "staff",
        Some("owner") => "owner",
        Some(other) => {
            return Err(
                Error::unprocessable(format!("unknown role {other:?}; use owner or staff"))
                    .with_code("invalid_role"),
            )
        }
    };
    let email = normalize_email(&s.email);
    let mut tx = tenant.begin_owner(&db).await?;
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
    let user_id = match existing {
        Some((id,)) => id,
        None => {
            let password = s.password.as_deref().ok_or_else(|| {
                Error::unprocessable("password is required for a new account")
                    .with_code("password_required")
            })?;
            create_user(&mut tx, &email, password, false)
                .await?
                .to_string()
        }
    };
    let added = sqlx::query(
        "INSERT INTO memberships (tenant_id, user_id, role) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(tenant.id())
    .bind(&user_id)
    .bind(role)
    .execute(&mut *tx)
    .await?;
    if added.rows_affected() == 0 {
        return Err(Error::conflict("already a member of this shop").with_code("already_member"));
    }
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(Member {
            user_id,
            email,
            role: role.to_owned(),
        }),
    ))
}

/// The shop's staff, owners first.
#[get("/api/staff", tag = "tenants")]
async fn list_staff(db: Db, tenant: Tenant) -> Result<Json<Vec<Member>>> {
    let mut tx = tenant.begin(&db).await?;
    let members = sqlx::query_as::<_, Member>(
        "SELECT m.user_id, u.email, m.role FROM memberships m \
         JOIN users u ON u.id = m.user_id \
         WHERE m.tenant_id = $1 ORDER BY m.role = 'owner' DESC, u.email",
    )
    .bind(tenant.id())
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(members))
}
