//! Custom fields.
//!
//! A shop defines fields per resource type (`PUT /api/fields/product/care`)
//! with a kind (text, integer, date, choice...). Modules that own a
//! resource expose its values (for products, `/api/products/{id}/fields`)
//! by calling [`values`], [`set_values`] and [`delete_values`] inside their
//! own tenant transaction, after checking the resource exists. The rules
//! for each kind are in [`soat_core::fields`].

use serde_json::{Map, Value};
use soat_core::fields::{check_choices, check_value, is_field_key, FieldKind};
use soat_core::Permission;
use soat_module::Module;
use soat_tenancy::{Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

/// See the shop's custom field definitions.
pub const FIELDS_READ: Permission =
    Permission::new("fields:read", "See the shop's custom field definitions").for_staff();
/// Define, change and delete custom fields.
pub const FIELDS_MANAGE: Permission =
    Permission::new("fields:manage", "Define, change and delete custom fields");

/// The fields module.
pub struct Fields;

impl Module for Fields {
    fn name(&self) -> &'static str {
        "fields"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[FIELDS_READ, FIELDS_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((list_definitions, put_definition, delete_definition))
    }
}

/// A custom field of a resource type.
#[model]
pub struct FieldDefinition {
    /// e.g. `product`.
    pub resource: String,
    /// e.g. `care_instructions`: a-z, 0-9 and `_`, starting with a letter.
    pub key: String,
    /// Shown to staff, e.g. `Care instructions`.
    pub name: String,
    /// `text`, `integer`, `decimal`, `boolean`, `date`, `url`, `choice`,
    /// `text_list` or `json`.
    pub kind: String,
    /// The allowed values of a `choice` field.
    pub choices: Vec<String>,
}

/// Body of `PUT /api/fields/{resource}/{key}`.
#[model]
pub struct DefineField {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub choices: Vec<String>,
}

type DefRow = (String, String, String, String, Vec<String>);

fn view(row: DefRow) -> FieldDefinition {
    let (resource, key, name, kind, choices) = row;
    FieldDefinition {
        resource,
        key,
        name,
        kind,
        choices,
    }
}

fn check_names(resource: &str, key: &str) -> Result<()> {
    if is_field_key(resource) && is_field_key(key) {
        Ok(())
    } else {
        Err(Error::not_found())
    }
}

/// The custom fields of a resource type, by key (`fields:read`).
#[get("/api/fields/{resource}", tag = "fields")]
async fn list_definitions(
    db: Db,
    tenant: Tenant,
    Path(resource): Path<String>,
) -> Result<Json<Vec<FieldDefinition>>> {
    let mut tx = tenant.begin_for(&db, &FIELDS_READ).await?;
    let rows: Vec<DefRow> = sqlx::query_as(
        "SELECT resource, key, name, kind, choices FROM field_definitions \
         WHERE tenant_id = $1 AND resource = $2 ORDER BY key",
    )
    .bind(tenant.id())
    .bind(&resource)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(rows.into_iter().map(view).collect()))
}

/// Define a custom field, or change its name or choices
/// (`fields:manage`). Changing the kind of a field that has values, or
/// dropping a choice in use, is a 409 `field_in_use`. Publishes
/// `field.defined`.
#[put("/api/fields/{resource}/{key}", tag = "fields")]
async fn put_definition(
    db: Db,
    tenant: Tenant,
    Path((resource, key)): Path<(String, String)>,
    Valid(Json(body)): Valid<Json<DefineField>>,
) -> Result<Json<FieldDefinition>> {
    if !is_field_key(&resource) || !is_field_key(&key) {
        return Err(Error::unprocessable(
            "resource and key are 1-64 of a-z, 0-9 and _, starting with a letter",
        )
        .with_code("invalid_field_key"));
    }
    let kind = FieldKind::parse(&body.kind).ok_or_else(|| {
        Error::unprocessable(format!("unknown kind `{}`", body.kind))
            .with_code("invalid_field_kind")
    })?;
    check_choices(kind, &body.choices)
        .map_err(|e| Error::unprocessable(e).with_code("invalid_choices"))?;
    let mut tx = tenant.begin_for(&db, &FIELDS_MANAGE).await?;
    let current: Option<(String,)> = sqlx::query_as(
        "SELECT kind FROM field_definitions WHERE tenant_id = $1 AND resource = $2 AND key = $3",
    )
    .bind(tenant.id())
    .bind(&resource)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_some() {
        let (stale,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM field_values WHERE tenant_id = $1 AND resource = $2 \
             AND key = $3 AND ($4 <> (SELECT kind FROM field_definitions \
             WHERE tenant_id = $1 AND resource = $2 AND key = $3) \
             OR ($4 = 'choice' AND NOT (value #>> '{}') = ANY($5)))",
        )
        .bind(tenant.id())
        .bind(&resource)
        .bind(&key)
        .bind(kind.as_str())
        .bind(&body.choices)
        .fetch_one(&mut *tx)
        .await?;
        if stale > 0 {
            return Err(Error::conflict(format!(
                "{stale} value(s) would no longer fit; clear them first"
            ))
            .with_code("field_in_use"));
        }
    }
    let row: DefRow = sqlx::query_as(
        "INSERT INTO field_definitions (tenant_id, resource, key, name, kind, choices) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (tenant_id, resource, key) DO UPDATE \
         SET name = EXCLUDED.name, kind = EXCLUDED.kind, choices = EXCLUDED.choices \
         RETURNING resource, key, name, kind, choices",
    )
    .bind(tenant.id())
    .bind(&resource)
    .bind(&key)
    .bind(&body.name)
    .bind(kind.as_str())
    .bind(&body.choices)
    .fetch_one(&mut *tx)
    .await?;
    let def = view(row);
    tenant.publish(&mut tx, "field.defined", &def).await?;
    tx.commit().await?;
    Ok(Json(def))
}

/// Delete a custom field and all its values (`fields:manage`). Publishes
/// `field.deleted`.
#[delete("/api/fields/{resource}/{key}", tag = "fields", status = 204)]
async fn delete_definition(
    db: Db,
    tenant: Tenant,
    Path((resource, key)): Path<(String, String)>,
) -> Result<StatusCode> {
    check_names(&resource, &key)?;
    let mut tx = tenant.begin_for(&db, &FIELDS_MANAGE).await?;
    let deleted = sqlx::query(
        "DELETE FROM field_definitions WHERE tenant_id = $1 AND resource = $2 AND key = $3",
    )
    .bind(tenant.id())
    .bind(&resource)
    .bind(&key)
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "field.deleted",
            &serde_json::json!({ "resource": resource, "key": key }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The custom field values of one resource, by key. Run it in the tenant
/// transaction after checking the resource exists.
pub async fn values(
    conn: &mut PgConnection,
    tenant: &Tenant,
    resource: &str,
    id: &str,
) -> Result<Map<String, Value>> {
    let rows: Vec<(String, sqlx::types::Json<Value>)> = sqlx::query_as(
        "SELECT key, value FROM field_values \
         WHERE tenant_id = $1 AND resource = $2 AND resource_id = $3 ORDER BY key",
    )
    .bind(tenant.id())
    .bind(resource)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(k, v)| (k, v.0)).collect())
}

/// Sets some custom field values of one resource; `null` clears a field.
/// An undefined key or a value of the wrong kind is a 422. Returns every
/// value after the change.
pub async fn set_values(
    tx: &mut TenantTx,
    tenant: &Tenant,
    resource: &str,
    id: &str,
    changes: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    let defs: Vec<(String, String, Vec<String>)> = sqlx::query_as(
        "SELECT key, kind, choices FROM field_definitions WHERE tenant_id = $1 AND resource = $2",
    )
    .bind(tenant.id())
    .bind(resource)
    .fetch_all(&mut **tx)
    .await?;
    for (key, value) in changes {
        let Some((_, kind, choices)) = defs.iter().find(|(k, _, _)| k == key) else {
            return Err(
                Error::unprocessable(format!("no custom field `{key}` on {resource}"))
                    .with_code("unknown_field"),
            );
        };
        if value.is_null() {
            sqlx::query(
                "DELETE FROM field_values WHERE tenant_id = $1 AND resource = $2 \
                 AND resource_id = $3 AND key = $4",
            )
            .bind(tenant.id())
            .bind(resource)
            .bind(id)
            .bind(key)
            .execute(&mut **tx)
            .await?;
            continue;
        }
        let kind = FieldKind::parse(kind).unwrap_or(FieldKind::Json);
        let value = check_value(kind, choices, value).map_err(|e| {
            Error::unprocessable(format!("{key}: {e}")).with_code("invalid_field_value")
        })?;
        sqlx::query(
            "INSERT INTO field_values (tenant_id, resource, resource_id, key, value) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, resource, resource_id, key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(tenant.id())
        .bind(resource)
        .bind(id)
        .bind(key)
        .bind(sqlx::types::Json(value))
        .execute(&mut **tx)
        .await?;
    }
    values(tx, tenant, resource, id).await
}

/// Deletes every custom field value of one resource, when it is deleted.
pub async fn delete_values(
    conn: &mut PgConnection,
    tenant: &Tenant,
    resource: &str,
    id: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM field_values WHERE tenant_id = $1 AND resource = $2 AND resource_id = $3",
    )
    .bind(tenant.id())
    .bind(resource)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
