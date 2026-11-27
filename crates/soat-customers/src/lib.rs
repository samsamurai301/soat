//! Customers.
//!
//! The people who buy from a shop: an email unique in the shop, names,
//! phone, tags, a staff note, marketing consent with the time it was given,
//! [`addresses`] and custom fields (`PUT /api/fields/customer/{key}`).
//! Customers sign in to the storefront with [`accounts`].

pub mod access;
pub mod accounts;
pub mod addresses;

use soat_auth::normalize_email;
use soat_core::tags::normalize_tags;
use soat_core::{id_kind, Permission};
use soat_module::Module;
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(CustomerId, CustomerKind, "cus");

/// See customers, their addresses and fields.
pub const CUSTOMERS_READ: Permission = Permission::new(
    "customers:read",
    "See customers, their addresses and fields",
)
.for_staff();
/// Create, change and delete customers.
pub const CUSTOMERS_WRITE: Permission =
    Permission::new("customers:write", "Create, change and delete customers").for_staff();

/// The customers module.
pub struct Customers;

impl Module for Customers {
    fn name(&self) -> &'static str {
        "customers"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "fields"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[CUSTOMERS_READ, CUSTOMERS_WRITE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_customers,
            create_customer,
            get_customer,
            update_customer,
            delete_customer,
            get_fields,
            set_fields,
        ))
        .plugin(addresses::mount)
        .plugin(accounts::mount)
        .plugin(access::mount)
    }
}

/// A customer.
#[model]
#[derive(sqlx::FromRow)]
pub struct Customer {
    /// e.g. `cus_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Lowercase, unique in the shop.
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub phone: String,
    pub accepts_marketing: bool,
    /// When consent was given (RFC 3339), or null.
    pub marketing_consent_at: Option<String>,
    pub tags: Vec<String>,
    /// For staff only.
    pub note: String,
    /// Whether they registered and can sign in.
    pub has_account: bool,
}

/// Body of a create request.
#[model]
pub struct NewCustomer {
    #[validate(email, length(max = 254))]
    pub email: String,
    #[validate(length(max = 100))]
    pub first_name: Option<String>,
    #[validate(length(max = 100))]
    pub last_name: Option<String>,
    #[validate(length(max = 40))]
    pub phone: Option<String>,
    pub accepts_marketing: Option<bool>,
    pub tags: Option<Vec<String>>,
    #[validate(length(max = 5000))]
    pub note: Option<String>,
}

/// Body of an update request; absent fields stay as they are.
#[model]
pub struct CustomerChange {
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
    #[validate(length(max = 100))]
    pub first_name: Option<String>,
    #[validate(length(max = 100))]
    pub last_name: Option<String>,
    #[validate(length(max = 40))]
    pub phone: Option<String>,
    pub accepts_marketing: Option<bool>,
    /// Replaces all tags.
    pub tags: Option<Vec<String>>,
    #[validate(length(max = 5000))]
    pub note: Option<String>,
}

const COLUMNS: &str = "id, email, first_name, last_name, phone, accepts_marketing, \
     to_char(marketing_consent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     AS marketing_consent_at, tags, note, password_hash IS NOT NULL AS has_account";

fn tags(raw: Option<&[String]>) -> Result<Option<Vec<String>>> {
    raw.map(|t| normalize_tags(t).map_err(|e| Error::unprocessable(e).with_code("invalid_tags")))
        .transpose()
}

/// One of the shop's customers, or 404.
pub async fn load(tx: &mut TenantTx, scope: &impl Scope, id: &str) -> Result<Customer> {
    let id = CustomerId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Customer>(&format!(
        "SELECT {COLUMNS} FROM customers WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's customers, newest first, 50 per page (`customers:read`).
/// `?q=` searches email and names; `?tag=vip` filters.
#[get("/api/customers", tag = "customers")]
async fn list_customers(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Customer>>> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_READ).await?;
    let page = TenantListing::new("customers", COLUMNS)
        .sortable(["email", "last_name"])
        .default_sort("-id")
        .search(["email", "first_name", "last_name"])
        .filter_array("tag", "tags")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Add a customer (`customers:write`). An email the shop already has is
/// a 409. Publishes `customer.created`.
#[post("/api/customers", tag = "customers", status = 201)]
async fn create_customer(
    db: Db,
    tenant: Tenant,
    Valid(Json(c)): Valid<Json<NewCustomer>>,
) -> Result<(StatusCode, Json<Customer>)> {
    let tags = tags(c.tags.as_deref())?.unwrap_or_default();
    let consent = c.accepts_marketing.unwrap_or(false);
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let customer = sqlx::query_as::<_, Customer>(&format!(
        "INSERT INTO customers (id, tenant_id, email, first_name, last_name, phone, \
         accepts_marketing, marketing_consent_at, tags, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $7 THEN now() END, $8, $9) \
         RETURNING {COLUMNS}"
    ))
    .bind(CustomerId::new().as_str())
    .bind(tenant.id())
    .bind(normalize_email(&c.email))
    .bind(c.first_name.unwrap_or_default())
    .bind(c.last_name.unwrap_or_default())
    .bind(c.phone.unwrap_or_default())
    .bind(consent)
    .bind(&tags)
    .bind(c.note.unwrap_or_default())
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "customer.created", &customer)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(customer)))
}

/// One customer (`customers:read`).
#[get("/api/customers/{id}", tag = "customers")]
async fn get_customer(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Customer>> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_READ).await?;
    let customer = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(customer))
}

/// Stops marketing email to customer `id`, who asked for it (e.g. with an
/// unsubscribe link), in the caller's transaction. Publishes
/// `customer.updated` when it changed anything; answers whether it did.
///
/// # Errors
///
/// Database errors.
pub async fn unsubscribe(tx: &mut TenantTx, scope: &impl Scope, id: &str) -> Result<bool> {
    let changed: Option<Customer> = sqlx::query_as(&format!(
        "UPDATE customers SET accepts_marketing = FALSE, marketing_consent_at = NULL \
         WHERE tenant_id = $1 AND id = $2 AND accepts_marketing RETURNING {COLUMNS}"
    ))
    .bind(scope.tenant_id())
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    match changed {
        Some(c) => {
            scope.publish(tx, "customer.updated", &c).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Change a customer (`customers:write`). Turning marketing on records the
/// time of consent; turning it off clears it. Publishes `customer.updated`.
#[patch("/api/customers/{id}", tag = "customers")]
async fn update_customer(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CustomerChange>>,
) -> Result<Json<Customer>> {
    let tags = tags(c.tags.as_deref())?;
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let customer = sqlx::query_as::<_, Customer>(&format!(
        "UPDATE customers SET email = COALESCE($3, email), \
         first_name = COALESCE($4, first_name), last_name = COALESCE($5, last_name), \
         phone = COALESCE($6, phone), \
         marketing_consent_at = CASE WHEN $7 IS NULL THEN marketing_consent_at \
             WHEN $7 THEN COALESCE(marketing_consent_at, now()) END, \
         accepts_marketing = COALESCE($7, accepts_marketing), \
         tags = COALESCE($8, tags), note = COALESCE($9, note) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(c.email.as_deref().map(normalize_email))
    .bind(&c.first_name)
    .bind(&c.last_name)
    .bind(&c.phone)
    .bind(c.accepts_marketing)
    .bind(&tags)
    .bind(&c.note)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "customer.updated", &customer)
        .await?;
    tx.commit().await?;
    Ok(Json(customer))
}

/// Delete a customer with their addresses and fields (`customers:write`).
/// Publishes `customer.deleted` with the id only.
#[delete("/api/customers/{id}", tag = "customers", status = 204)]
async fn delete_customer(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM customers WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    soat_fields::delete_values(&mut tx, &tenant, "customer", &current.id).await?;
    tenant
        .publish(
            &mut tx,
            "customer.deleted",
            &serde_json::json!({ "id": current.id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The customer's custom field values (`customers:read`).
#[get("/api/customers/{id}/fields", tag = "customers")]
async fn get_fields(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_READ).await?;
    let customer = load(&mut tx, &tenant, &id).await?;
    let values = soat_fields::values(&mut tx, &tenant, "customer", &customer.id).await?;
    tx.commit().await?;
    Ok(Json(values))
}

/// Set some of the customer's custom field values; `null` clears one
/// (`customers:write`). Publishes `customer.fields_changed`.
#[patch("/api/customers/{id}/fields", tag = "customers")]
async fn set_fields(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Json(changes): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<serde_json::Map<String, serde_json::Value>>> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let customer = load(&mut tx, &tenant, &id).await?;
    let values =
        soat_fields::set_values(&mut tx, &tenant, "customer", &customer.id, &changes).await?;
    tenant
        .publish(
            &mut tx,
            "customer.fields_changed",
            &serde_json::json!({ "customer_id": customer.id, "fields": values }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(values))
}
