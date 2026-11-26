//! Stock locations.

use soat_core::address::is_country_code;
use soat_core::id_kind;
use soat_tenancy::{Tenant, TenantListing, TenantTx};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{INVENTORY_READ, LOCATIONS_MANAGE};

id_kind!(LocationId, LocationKind, "loc");

/// A place that holds stock.
#[model]
#[derive(sqlx::FromRow)]
pub struct Location {
    /// e.g. `loc_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, e.g. `Berlin warehouse`.
    pub name: String,
    pub address_line1: String,
    pub address_line2: String,
    pub postal_code: String,
    pub city: String,
    /// ISO 3166-1 alpha-2, e.g. `DE`.
    pub country: String,
    /// Inactive locations keep their stock but take no new orders.
    pub active: bool,
}

/// Body of a create request.
#[model]
pub struct NewLocation {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[validate(length(max = 200))]
    pub address_line1: Option<String>,
    #[validate(length(max = 200))]
    pub address_line2: Option<String>,
    #[validate(length(max = 20))]
    pub postal_code: Option<String>,
    #[validate(length(max = 100))]
    pub city: Option<String>,
    pub country: String,
}

/// Body of an update request; absent fields stay as they are.
#[model]
pub struct LocationChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[validate(length(max = 200))]
    pub address_line1: Option<String>,
    #[validate(length(max = 200))]
    pub address_line2: Option<String>,
    #[validate(length(max = 20))]
    pub postal_code: Option<String>,
    #[validate(length(max = 100))]
    pub city: Option<String>,
    pub country: Option<String>,
    pub active: Option<bool>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((
        list_locations,
        create_location,
        get_location,
        update_location,
        delete_location,
    ))
}

const COLUMNS: &str = "id, name, address_line1, address_line2, postal_code, city, country, active";

fn check_country(country: &str) -> Result<()> {
    if is_country_code(country) {
        Ok(())
    } else {
        Err(
            Error::unprocessable("country is an ISO 3166-1 alpha-2 code such as DE")
                .with_code("invalid_country"),
        )
    }
}

/// One of the shop's locations, or 404.
pub async fn load(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Location> {
    let id = LocationId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Location>(&format!(
        "SELECT {COLUMNS} FROM locations WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's locations (`inventory:read`). `?q=` searches name and city;
/// `?country=DE` filters.
#[get("/api/locations", tag = "inventory")]
async fn list_locations(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Location>>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let page = TenantListing::new("locations", COLUMNS)
        .sortable(["name", "city", "country"])
        .search(["name", "city"])
        .filter_text("country")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Add a location (`locations:manage`). A name the shop already uses is a
/// 409. Publishes `location.created`.
#[post("/api/locations", tag = "inventory", status = 201)]
async fn create_location(
    db: Db,
    tenant: Tenant,
    Valid(Json(body)): Valid<Json<NewLocation>>,
) -> Result<(StatusCode, Json<Location>)> {
    check_country(&body.country)?;
    let mut tx = tenant.begin_for(&db, &LOCATIONS_MANAGE).await?;
    let location = sqlx::query_as::<_, Location>(&format!(
        "INSERT INTO locations \
         (id, tenant_id, name, address_line1, address_line2, postal_code, city, country) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLUMNS}"
    ))
    .bind(LocationId::new().as_str())
    .bind(tenant.id())
    .bind(&body.name)
    .bind(body.address_line1.unwrap_or_default())
    .bind(body.address_line2.unwrap_or_default())
    .bind(body.postal_code.unwrap_or_default())
    .bind(body.city.unwrap_or_default())
    .bind(&body.country)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "location.created", &location)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(location)))
}

/// One location (`inventory:read`).
#[get("/api/locations/{id}", tag = "inventory")]
async fn get_location(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Location>> {
    let mut tx = tenant.begin_for(&db, &INVENTORY_READ).await?;
    let location = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(location))
}

/// Change a location, or deactivate it (`locations:manage`). Publishes
/// `location.updated`.
#[patch("/api/locations/{id}", tag = "inventory")]
async fn update_location(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<LocationChange>>,
) -> Result<Json<Location>> {
    if let Some(country) = &c.country {
        check_country(country)?;
    }
    let mut tx = tenant.begin_for(&db, &LOCATIONS_MANAGE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let location = sqlx::query_as::<_, Location>(&format!(
        "UPDATE locations SET name = COALESCE($3, name), \
         address_line1 = COALESCE($4, address_line1), address_line2 = COALESCE($5, address_line2), \
         postal_code = COALESCE($6, postal_code), city = COALESCE($7, city), \
         country = COALESCE($8, country), active = COALESCE($9, active) \
         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&current.id)
    .bind(&c.name)
    .bind(&c.address_line1)
    .bind(&c.address_line2)
    .bind(&c.postal_code)
    .bind(&c.city)
    .bind(&c.country)
    .bind(c.active)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "location.updated", &location)
        .await?;
    tx.commit().await?;
    Ok(Json(location))
}

/// Delete a location that holds no stock (`locations:manage`); one with
/// stock is a 409 `location_has_stock`: move or write it off first.
/// Publishes `location.deleted`.
#[delete("/api/locations/{id}", tag = "inventory", status = 204)]
async fn delete_location(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &LOCATIONS_MANAGE).await?;
    let current = load(&mut tx, &tenant, &id).await?;
    let (units,): (Option<i64>,) = sqlx::query_as(
        "SELECT SUM(on_hand)::BIGINT FROM inventory_levels WHERE tenant_id = $1 AND location_id = $2",
    )
    .bind(tenant.id())
    .bind(&current.id)
    .fetch_one(&mut *tx)
    .await?;
    if units.unwrap_or(0) > 0 {
        return Err(
            Error::conflict("this location still holds stock").with_code("location_has_stock")
        );
    }
    sqlx::query("DELETE FROM locations WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "location.deleted", &current)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
