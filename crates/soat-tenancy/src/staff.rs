//! A shop's staff, roles and the permissions roles grant.
//!
//! Nobody can hand out more than they have: granting a role needs every
//! permission in it, and only owners make owners or edit roles.

use soat_auth::{create_user, normalize_email};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{Access, Tenant, OWNER, STAFF, STAFF_MANAGE, STAFF_READ};

/// Someone to add to the shop's staff.
#[model]
pub struct NewStaff {
    #[validate(email)]
    pub email: String,
    /// Required when no account exists for this email yet (at least 12
    /// characters); ignored when one does.
    #[validate(length(min = 12, max = 1024))]
    pub password: Option<String>,
    /// `owner`, `staff` (the default) or a custom role.
    pub role: Option<String>,
}

/// A new role for a member.
#[model]
pub struct RoleChange {
    pub role: String,
}

/// A member of a shop's staff.
#[model]
#[derive(sqlx::FromRow)]
pub struct Member {
    pub user_id: String,
    pub email: String,
    pub role: String,
}

/// A role and what it grants. Built-in roles cannot be changed.
#[model]
pub struct RoleView {
    pub name: String,
    pub built_in: bool,
    /// Permission names; `["*"]` for owner.
    pub permissions: Vec<String>,
}

/// The permissions a custom role grants.
#[model]
pub struct RoleBody {
    pub permissions: Vec<String>,
}

/// A permission a role can grant.
#[model]
pub struct PermissionView {
    pub name: String,
    pub description: String,
    /// Part of the built-in `staff` role.
    pub staff_default: bool,
}

pub fn mount(app: App) -> App {
    app.mount((
        add_staff,
        list_staff,
        change_role,
        list_permissions,
        list_roles,
        put_role,
        delete_role,
    ))
}

/// 422 unless `role` exists here; 403 unless `access` holds everything
/// the role grants (only owners grant `owner`).
async fn check_grant(
    tenant: &Tenant,
    conn: &mut PgConnection,
    access: &Access,
    role: &str,
) -> Result<()> {
    if role == OWNER {
        return if access.is_owner() {
            Ok(())
        } else {
            Err(Error::forbidden().with_code("owner_only"))
        };
    }
    if role != STAFF {
        let exists: Option<(String,)> = sqlx::query_as("SELECT name FROM roles WHERE name = $1")
            .bind(role)
            .fetch_optional(&mut *conn)
            .await?;
        if exists.is_none() {
            return Err(
                Error::unprocessable(format!("no role named {role:?}")).with_code("invalid_role")
            );
        }
    }
    let granted = tenant.resolve(conn, role.to_owned()).await?;
    let beyond: Vec<_> = tenant
        .permissions()
        .all()
        .iter()
        .filter(|p| granted.allows(p.name) && !access.allows(p.name))
        .map(|p| p.name)
        .collect();
    if beyond.is_empty() {
        Ok(())
    } else {
        Err(Error::new(
            StatusCode::FORBIDDEN,
            format!(
                "this role grants permissions you lack: {}",
                beyond.join(", ")
            ),
        )
        .with_code("beyond_own_permissions"))
    }
}

/// Add someone to the shop's staff (`staff:manage`). Creates their account
/// when the email is new (then `password` is required); a person who is
/// already a member is a 409.
#[post("/api/staff", tag = "staff", status = 201)]
async fn add_staff(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<NewStaff>>,
) -> Result<(StatusCode, Json<Member>)> {
    let role = s.role.clone().unwrap_or_else(|| STAFF.to_owned());
    let email = normalize_email(&s.email);
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&STAFF_MANAGE)?;
    check_grant(&tenant, &mut tx, &access, &role).await?;
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
    .bind(&role)
    .execute(&mut *tx)
    .await?;
    if added.rows_affected() == 0 {
        return Err(Error::conflict("already a member of this shop").with_code("already_member"));
    }
    let member = Member {
        user_id,
        email,
        role,
    };
    tenant.publish(&mut tx, "staff.added", &member).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(member)))
}

/// The shop's staff, owners first (`staff:read`).
#[get("/api/staff", tag = "staff")]
async fn list_staff(db: Db, tenant: Tenant) -> Result<Json<Vec<Member>>> {
    let mut tx = tenant.begin_for(&db, &STAFF_READ).await?;
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

/// Change a member's role (`staff:manage`). You cannot change your own
/// role, and only owners change an owner's.
#[patch("/api/staff/{user_id}", tag = "staff")]
async fn change_role(
    db: Db,
    tenant: Tenant,
    Path(user_id): Path<String>,
    Json(change): Json<RoleChange>,
) -> Result<Json<Member>> {
    if tenant
        .staff()
        .is_some_and(|s| s.user_id.as_str() == user_id)
    {
        return Err(Error::forbidden().with_code("own_role"));
    }
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    access.require(&STAFF_MANAGE)?;
    let current: Option<(String, String)> = sqlx::query_as(
        "SELECT m.role, u.email FROM memberships m JOIN users u ON u.id = m.user_id \
         WHERE m.user_id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((current, email)) = current else {
        return Err(Error::not_found());
    };
    if current == OWNER && !access.is_owner() {
        return Err(Error::forbidden().with_code("owner_only"));
    }
    check_grant(&tenant, &mut tx, &access, &change.role).await?;
    sqlx::query("UPDATE memberships SET role = $1 WHERE user_id = $2")
        .bind(&change.role)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(
            &mut tx,
            "staff.role_changed",
            &serde_json::json!({ "user_id": user_id, "from": current, "to": change.role }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(Member {
        user_id,
        email,
        role: change.role,
    }))
}

/// Every permission a role can grant in this build.
#[get("/api/permissions", tag = "staff")]
async fn list_permissions(db: Db, tenant: Tenant) -> Result<Json<Vec<PermissionView>>> {
    tenant.begin(&db).await?.commit().await?;
    Ok(Json(
        tenant
            .permissions()
            .all()
            .iter()
            .map(|p| PermissionView {
                name: p.name.to_owned(),
                description: p.description.to_owned(),
                staff_default: p.staff_default,
            })
            .collect(),
    ))
}

/// Built-in and custom roles (`staff:read`).
#[get("/api/roles", tag = "staff")]
async fn list_roles(db: Db, tenant: Tenant) -> Result<Json<Vec<RoleView>>> {
    let mut tx = tenant.begin_for(&db, &STAFF_READ).await?;
    let custom: Vec<(String, Vec<String>)> =
        sqlx::query_as("SELECT name, permissions FROM roles ORDER BY name")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    let mut roles = vec![
        RoleView {
            name: OWNER.to_owned(),
            built_in: true,
            permissions: vec!["*".to_owned()],
        },
        RoleView {
            name: STAFF.to_owned(),
            built_in: true,
            permissions: tenant
                .permissions()
                .staff_defaults()
                .map(str::to_owned)
                .collect(),
        },
    ];
    roles.extend(custom.into_iter().map(|(name, permissions)| RoleView {
        name,
        built_in: false,
        permissions,
    }));
    Ok(Json(roles))
}

/// Create or replace a custom role. Owners only. Names are 2 to 40 of
/// a-z, 0-9, - and _; every permission must exist.
#[put("/api/roles/{name}", tag = "staff")]
async fn put_role(
    db: Db,
    tenant: Tenant,
    Path(name): Path<String>,
    Json(body): Json<RoleBody>,
) -> Result<Json<RoleView>> {
    let valid_name = (2..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !valid_name || name == OWNER || name == STAFF {
        return Err(
            Error::unprocessable(format!("{name:?} cannot be a custom role name"))
                .with_code("invalid_role"),
        );
    }
    if let Some(unknown) = body
        .permissions
        .iter()
        .find(|p| tenant.permissions().get(p).is_none())
    {
        return Err(
            Error::unprocessable(format!("unknown permission {unknown:?}"))
                .with_code("unknown_permission"),
        );
    }
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    if !access.is_owner() {
        return Err(Error::forbidden().with_code("owner_only"));
    }
    let mut permissions = body.permissions;
    permissions.sort();
    permissions.dedup();
    sqlx::query(
        "INSERT INTO roles (tenant_id, name, permissions) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id, name) DO UPDATE SET permissions = EXCLUDED.permissions",
    )
    .bind(tenant.id())
    .bind(&name)
    .bind(&permissions)
    .execute(&mut *tx)
    .await?;
    let role = RoleView {
        name,
        built_in: false,
        permissions,
    };
    tenant.publish(&mut tx, "role.updated", &role).await?;
    tx.commit().await?;
    Ok(Json(role))
}

/// Delete a custom role. Owners only; a role someone still has is a 409.
#[delete("/api/roles/{name}", tag = "staff", status = 204)]
async fn delete_role(db: Db, tenant: Tenant, Path(name): Path<String>) -> Result<StatusCode> {
    let (mut tx, access) = tenant.begin_with_access(&db).await?;
    if !access.is_owner() {
        return Err(Error::forbidden().with_code("owner_only"));
    }
    let (in_use,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memberships WHERE role = $1")
        .bind(&name)
        .fetch_one(&mut *tx)
        .await?;
    if in_use > 0 {
        return Err(
            Error::conflict(format!("{in_use} member(s) still have this role"))
                .with_code("role_in_use"),
        );
    }
    let deleted = sqlx::query("DELETE FROM roles WHERE name = $1")
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "role.deleted",
            &serde_json::json!({ "name": name }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
