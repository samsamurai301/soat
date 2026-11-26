//! Shipping zones: the countries a shop ships to, grouped by how it
//! charges.

use soat_core::address::country_list;
use soat_core::id_kind;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{ShippingRate, RATE_COLUMNS, SHIPPING_MANAGE, SHIPPING_READ};

id_kind!(ShippingZoneId, ShippingZoneKind, "shz");

/// A shipping zone with its countries and rates.
#[model]
pub struct ShippingZone {
    /// e.g. `shz_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, e.g. `Domestic` or `EU`.
    pub name: String,
    /// ISO 3166-1 alpha-2 codes, sorted. A country is in at most one zone.
    pub countries: Vec<String>,
    /// Cheapest first.
    pub rates: Vec<ShippingRate>,
}

/// Body of a zone creation.
#[model]
pub struct NewZone {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub countries: Vec<String>,
}

/// Body of a zone change; a given country list replaces the old one.
#[model]
pub struct ZoneChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub countries: Option<Vec<String>>,
}

/// The zones of a shop.
#[model]
pub struct ZoneList {
    pub items: Vec<ShippingZone>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((list_zones, create_zone, get_zone, update_zone, delete_zone))
}

fn countries(raw: &[String]) -> Result<Vec<String>> {
    country_list(raw).map_err(|e| Error::unprocessable(e).with_code("invalid_country"))
}

/// Replaces a zone's countries; a country in another zone is a 409.
async fn set_countries(
    conn: &mut PgConnection,
    scope: &impl Scope,
    zone: &str,
    countries: &[String],
) -> Result<()> {
    sqlx::query("DELETE FROM shipping_zone_countries WHERE tenant_id = $1 AND zone_id = $2")
        .bind(scope.tenant_id())
        .bind(zone)
        .execute(&mut *conn)
        .await?;
    let taken: Vec<(String,)> = sqlx::query_as(
        "SELECT country FROM shipping_zone_countries WHERE tenant_id = $1 AND country = ANY($2) \
         ORDER BY country",
    )
    .bind(scope.tenant_id())
    .bind(countries)
    .fetch_all(&mut *conn)
    .await?;
    if !taken.is_empty() {
        let list: Vec<String> = taken.into_iter().map(|(c,)| c).collect();
        return Err(
            Error::conflict(format!("already in another zone: {}", list.join(", ")))
                .with_code("country_in_other_zone"),
        );
    }
    sqlx::query(
        "INSERT INTO shipping_zone_countries (tenant_id, zone_id, country) \
         SELECT $1, $2, unnest($3::TEXT[])",
    )
    .bind(scope.tenant_id())
    .bind(zone)
    .bind(countries)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One zone with its rates, or 404.
async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<ShippingZone> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT id, name FROM shipping_zones WHERE tenant_id = $1 AND id = $2")
            .bind(scope.tenant_id())
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let (id, name) = row.ok_or_else(Error::not_found)?;
    let countries: Vec<(String,)> = sqlx::query_as(
        "SELECT country FROM shipping_zone_countries WHERE tenant_id = $1 AND zone_id = $2 \
         ORDER BY country",
    )
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    let rates = sqlx::query_as::<_, ShippingRate>(&format!(
        "SELECT {RATE_COLUMNS} FROM shipping_rates WHERE tenant_id = $1 AND zone_id = $2 \
         ORDER BY price_minor, name, id"
    ))
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(ShippingZone {
        id,
        name,
        countries: countries.into_iter().map(|(c,)| c).collect(),
        rates,
    })
}

fn parse(id: &str) -> Result<String> {
    ShippingZoneId::parse(id)
        .map(|i| i.to_string())
        .map_err(|_| Error::not_found())
}

/// The shop's shipping zones with their rates, by name (`shipping:read`).
#[get("/api/shipping/zones", tag = "shipping")]
async fn list_zones(db: Db, tenant: Tenant) -> Result<Json<ZoneList>> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_READ).await?;
    let ids: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM shipping_zones WHERE tenant_id = $1 ORDER BY name, id")
            .bind(tenant.id())
            .fetch_all(&mut *tx)
            .await?;
    let mut items = Vec::with_capacity(ids.len());
    for (id,) in ids {
        items.push(load(&mut tx, &tenant, &id).await?);
    }
    tx.commit().await?;
    Ok(Json(ZoneList { items }))
}

/// Create a shipping zone (`shipping:manage`); add rates with
/// `POST /api/shipping/zones/{id}/rates`. A name the shop already uses, or
/// a country already in another zone, is a 409. Publishes
/// `shipping_zone.created`.
#[post("/api/shipping/zones", tag = "shipping", status = 201)]
async fn create_zone(
    db: Db,
    tenant: Tenant,
    Valid(Json(z)): Valid<Json<NewZone>>,
) -> Result<(StatusCode, Json<ShippingZone>)> {
    let countries = countries(&z.countries)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let id = ShippingZoneId::new();
    sqlx::query("INSERT INTO shipping_zones (id, tenant_id, name) VALUES ($1, $2, $3)")
        .bind(id.as_str())
        .bind(tenant.id())
        .bind(z.name.trim())
        .execute(&mut *tx)
        .await?;
    set_countries(&mut tx, &tenant, id.as_str(), &countries).await?;
    let zone = load(&mut tx, &tenant, id.as_str()).await?;
    tenant
        .publish(&mut tx, "shipping_zone.created", &zone)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(zone)))
}

/// One shipping zone with its rates (`shipping:read`).
#[get("/api/shipping/zones/{id}", tag = "shipping")]
async fn get_zone(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<ShippingZone>> {
    let id = parse(&id)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_READ).await?;
    let zone = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(zone))
}

/// Rename a zone or replace its countries (`shipping:manage`). Publishes
/// `shipping_zone.updated`.
#[patch("/api/shipping/zones/{id}", tag = "shipping")]
async fn update_zone(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(z)): Valid<Json<ZoneChange>>,
) -> Result<Json<ShippingZone>> {
    let id = parse(&id)?;
    let new_countries = z.countries.as_deref().map(countries).transpose()?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let found = sqlx::query(
        "UPDATE shipping_zones SET name = COALESCE($3, name) WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&id)
    .bind(z.name.as_deref().map(str::trim))
    .execute(&mut *tx)
    .await?;
    if found.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    if let Some(c) = &new_countries {
        set_countries(&mut tx, &tenant, &id, c).await?;
    }
    let zone = load(&mut tx, &tenant, &id).await?;
    tenant
        .publish(&mut tx, "shipping_zone.updated", &zone)
        .await?;
    tx.commit().await?;
    Ok(Json(zone))
}

/// Delete a zone and its rates; its countries can no longer be shipped to
/// (`shipping:manage`). Publishes `shipping_zone.deleted`.
#[delete("/api/shipping/zones/{id}", tag = "shipping", status = 204)]
async fn delete_zone(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = parse(&id)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM shipping_zones WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "shipping_zone.deleted",
            &serde_json::json!({ "id": id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
