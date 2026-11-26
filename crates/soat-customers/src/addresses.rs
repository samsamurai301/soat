//! A customer's addresses. At most one is the default.

use soat_core::address::is_country_code;
use soat_core::id_kind;
use soat_tenancy::{Tenant, TenantTx};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{load as load_customer, CUSTOMERS_READ, CUSTOMERS_WRITE};

id_kind!(AddressId, AddressKind, "addr");

/// Most addresses one customer keeps.
pub const MAX_ADDRESSES: i64 = 50;

/// A postal address of a customer.
#[model]
#[derive(sqlx::FromRow)]
pub struct Address {
    /// e.g. `addr_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub first_name: String,
    pub last_name: String,
    pub company: String,
    pub line1: String,
    pub line2: String,
    pub postal_code: String,
    pub city: String,
    /// State or province, where the country uses one.
    pub region: String,
    /// ISO 3166-1 alpha-2, e.g. `DE`.
    pub country: String,
    pub phone: String,
    pub is_default: bool,
}

/// Body of a create request, and of a full replace.
#[model]
pub struct AddressInput {
    #[validate(length(max = 100))]
    pub first_name: Option<String>,
    #[validate(length(max = 100))]
    pub last_name: Option<String>,
    #[validate(length(max = 200))]
    pub company: Option<String>,
    #[validate(length(min = 1, max = 200))]
    pub line1: String,
    #[validate(length(max = 200))]
    pub line2: Option<String>,
    #[validate(length(max = 20))]
    pub postal_code: Option<String>,
    #[validate(length(min = 1, max = 100))]
    pub city: String,
    #[validate(length(max = 100))]
    pub region: Option<String>,
    pub country: String,
    #[validate(length(max = 40))]
    pub phone: Option<String>,
    /// Make this the default address.
    #[serde(default)]
    pub is_default: bool,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((list_addresses, add_address, replace_address, delete_address))
}

const COLUMNS: &str = "id, first_name, last_name, company, line1, line2, postal_code, city, \
     region, country, phone, is_default";

fn check(a: &AddressInput) -> Result<()> {
    if is_country_code(&a.country) {
        Ok(())
    } else {
        Err(
            Error::unprocessable("country is an ISO 3166-1 alpha-2 code such as DE")
                .with_code("invalid_country"),
        )
    }
}

async fn clear_default(tx: &mut TenantTx, tenant: &Tenant, customer: &str) -> Result<()> {
    sqlx::query(
        "UPDATE customer_addresses SET is_default = FALSE \
         WHERE tenant_id = $1 AND customer_id = $2 AND is_default",
    )
    .bind(tenant.id())
    .bind(customer)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The customer's addresses, the default first (`customers:read`).
#[get("/api/customers/{id}/addresses", tag = "customers")]
async fn list_addresses(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Vec<Address>>> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_READ).await?;
    let customer = load_customer(&mut tx, &tenant, &id).await?;
    let addresses = sqlx::query_as::<_, Address>(&format!(
        "SELECT {COLUMNS} FROM customer_addresses WHERE tenant_id = $1 AND customer_id = $2 \
         ORDER BY is_default DESC, id"
    ))
    .bind(tenant.id())
    .bind(&customer.id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(addresses))
}

/// Add an address (`customers:write`). The first one becomes the default.
/// Publishes `customer.address_added`.
#[post("/api/customers/{id}/addresses", tag = "customers", status = 201)]
async fn add_address(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(a)): Valid<Json<AddressInput>>,
) -> Result<(StatusCode, Json<Address>)> {
    check(&a)?;
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let customer = load_customer(&mut tx, &tenant, &id).await?;
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM customer_addresses WHERE tenant_id = $1 AND customer_id = $2",
    )
    .bind(tenant.id())
    .bind(&customer.id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_ADDRESSES {
        return Err(
            Error::conflict(format!("at most {MAX_ADDRESSES} addresses"))
                .with_code("too_many_addresses"),
        );
    }
    let is_default = a.is_default || count == 0;
    if is_default {
        clear_default(&mut tx, &tenant, &customer.id).await?;
    }
    let address = sqlx::query_as::<_, Address>(&format!(
        "INSERT INTO customer_addresses (id, tenant_id, customer_id, first_name, last_name, \
         company, line1, line2, postal_code, city, region, country, phone, is_default) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING {COLUMNS}"
    ))
    .bind(AddressId::new().as_str())
    .bind(tenant.id())
    .bind(&customer.id)
    .bind(a.first_name.unwrap_or_default())
    .bind(a.last_name.unwrap_or_default())
    .bind(a.company.unwrap_or_default())
    .bind(&a.line1)
    .bind(a.line2.unwrap_or_default())
    .bind(a.postal_code.unwrap_or_default())
    .bind(&a.city)
    .bind(a.region.unwrap_or_default())
    .bind(&a.country)
    .bind(a.phone.unwrap_or_default())
    .bind(is_default)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(
            &mut tx,
            "customer.address_added",
            &serde_json::json!({ "customer_id": customer.id, "address": address }),
        )
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(address)))
}

/// Replace an address (`customers:write`). Publishes
/// `customer.address_updated`.
#[put("/api/customers/{id}/addresses/{address_id}", tag = "customers")]
async fn replace_address(
    db: Db,
    tenant: Tenant,
    Path((id, address_id)): Path<(String, String)>,
    Valid(Json(a)): Valid<Json<AddressInput>>,
) -> Result<Json<Address>> {
    check(&a)?;
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let customer = load_customer(&mut tx, &tenant, &id).await?;
    if a.is_default {
        clear_default(&mut tx, &tenant, &customer.id).await?;
    }
    let address = sqlx::query_as::<_, Address>(&format!(
        "UPDATE customer_addresses SET first_name = $4, last_name = $5, company = $6, \
         line1 = $7, line2 = $8, postal_code = $9, city = $10, region = $11, country = $12, \
         phone = $13, is_default = is_default OR $14 \
         WHERE tenant_id = $1 AND customer_id = $2 AND id = $3 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&customer.id)
    .bind(&address_id)
    .bind(a.first_name.unwrap_or_default())
    .bind(a.last_name.unwrap_or_default())
    .bind(a.company.unwrap_or_default())
    .bind(&a.line1)
    .bind(a.line2.unwrap_or_default())
    .bind(a.postal_code.unwrap_or_default())
    .bind(&a.city)
    .bind(a.region.unwrap_or_default())
    .bind(&a.country)
    .bind(a.phone.unwrap_or_default())
    .bind(a.is_default)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Error::not_found)?;
    tenant
        .publish(
            &mut tx,
            "customer.address_updated",
            &serde_json::json!({ "customer_id": customer.id, "address": address }),
        )
        .await?;
    tx.commit().await?;
    Ok(Json(address))
}

/// Delete an address (`customers:write`). When it was the default, the
/// oldest remaining one becomes the default. Publishes
/// `customer.address_deleted`.
#[delete(
    "/api/customers/{id}/addresses/{address_id}",
    tag = "customers",
    status = 204
)]
async fn delete_address(
    db: Db,
    tenant: Tenant,
    Path((id, address_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &CUSTOMERS_WRITE).await?;
    let customer = load_customer(&mut tx, &tenant, &id).await?;
    let gone: Option<(bool,)> = sqlx::query_as(
        "DELETE FROM customer_addresses WHERE tenant_id = $1 AND customer_id = $2 AND id = $3 \
         RETURNING is_default",
    )
    .bind(tenant.id())
    .bind(&customer.id)
    .bind(&address_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((was_default,)) = gone else {
        return Err(Error::not_found());
    };
    if was_default {
        sqlx::query(
            "UPDATE customer_addresses SET is_default = TRUE WHERE id = \
             (SELECT id FROM customer_addresses WHERE tenant_id = $1 AND customer_id = $2 \
              ORDER BY id LIMIT 1)",
        )
        .bind(tenant.id())
        .bind(&customer.id)
        .execute(&mut *tx)
        .await?;
    }
    tenant
        .publish(
            &mut tx,
            "customer.address_deleted",
            &serde_json::json!({ "customer_id": customer.id, "id": address_id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
