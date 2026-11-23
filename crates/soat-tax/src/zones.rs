//! Tax zones: countries sharing rates per tax class.

use soat_core::address::is_country_code;
use soat_core::id_kind;
use soat_core::tax::{is_tax_class, MAX_RATE_BPS};
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::{TAX_MANAGE, TAX_READ};

id_kind!(TaxZoneId, TaxZoneKind, "txz");

/// Most countries in one zone.
pub const MAX_COUNTRIES: usize = 300;
/// Most rates in one zone.
pub const MAX_RATES: usize = 50;

/// A tax zone with its countries and rates.
#[model]
pub struct TaxZone {
    /// e.g. `txz_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, e.g. `Germany` or `Rest of world`.
    pub name: String,
    /// ISO 3166-1 alpha-2 codes, sorted. A country is in at most one zone.
    pub countries: Vec<String>,
    /// Rates by tax class, sorted by class.
    pub rates: Vec<TaxRate>,
}

/// A zone's rate for one tax class.
#[model]
#[derive(sqlx::FromRow)]
pub struct TaxRate {
    /// e.g. `standard` or `reduced`.
    pub tax_class: String,
    /// Basis points: 1900 is 19%, at most 10000.
    pub rate_bps: i64,
    /// Shown on invoices, e.g. `MwSt. 19%`.
    #[serde(default)]
    pub name: String,
}

/// Body of a zone creation.
#[model]
pub struct NewZone {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub countries: Vec<String>,
    pub rates: Vec<TaxRate>,
}

/// Body of a zone change; given lists replace the old ones.
#[model]
pub struct ZoneChange {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub countries: Option<Vec<String>>,
    pub rates: Option<Vec<TaxRate>>,
}

/// The zones of a shop.
#[model]
pub struct ZoneList {
    pub items: Vec<TaxZone>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((list_zones, create_zone, get_zone, update_zone, delete_zone))
}

pub(crate) fn countries(raw: &[String]) -> Result<Vec<String>> {
    if raw.len() > MAX_COUNTRIES {
        return Err(
            Error::unprocessable(format!("at most {MAX_COUNTRIES} countries"))
                .with_code("invalid_countries"),
        );
    }
    let mut out = Vec::with_capacity(raw.len());
    for c in raw {
        let c = c.trim().to_ascii_uppercase();
        if !is_country_code(&c) {
            return Err(Error::unprocessable(format!(
                "{c} is not an ISO 3166-1 alpha-2 country code"
            ))
            .with_code("invalid_country"));
        }
        out.push(c);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub(crate) fn rates(raw: &[TaxRate]) -> Result<Vec<TaxRate>> {
    if raw.len() > MAX_RATES {
        return Err(
            Error::unprocessable(format!("at most {MAX_RATES} rates")).with_code("invalid_rates")
        );
    }
    let mut out: Vec<TaxRate> = Vec::with_capacity(raw.len());
    for r in raw {
        if !is_tax_class(&r.tax_class) {
            return Err(
                Error::unprocessable(format!("{} is not a tax class", r.tax_class))
                    .with_code("invalid_tax_class"),
            );
        }
        if !(0..=MAX_RATE_BPS).contains(&r.rate_bps) {
            return Err(
                Error::unprocessable("a rate is 0 to 10000 basis points").with_code("invalid_rate")
            );
        }
        if r.name.chars().count() > 100 {
            return Err(
                Error::unprocessable("a rate's name is at most 100 characters")
                    .with_code("invalid_rate"),
            );
        }
        if out.iter().any(|o| o.tax_class == r.tax_class) {
            return Err(
                Error::unprocessable(format!("{} is rated twice", r.tax_class))
                    .with_code("invalid_rates"),
            );
        }
        out.push(r.clone());
    }
    out.sort_by(|a, b| a.tax_class.cmp(&b.tax_class));
    Ok(out)
}

/// Replaces a zone's countries; a country in another zone is a 409.
pub(crate) async fn set_countries(
    conn: &mut PgConnection,
    scope: &impl Scope,
    zone: &str,
    countries: &[String],
) -> Result<()> {
    sqlx::query("DELETE FROM tax_zone_countries WHERE tenant_id = $1 AND zone_id = $2")
        .bind(scope.tenant_id())
        .bind(zone)
        .execute(&mut *conn)
        .await?;
    let taken: Vec<(String,)> = sqlx::query_as(
        "SELECT country FROM tax_zone_countries WHERE tenant_id = $1 AND country = ANY($2) \
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
        "INSERT INTO tax_zone_countries (tenant_id, zone_id, country) \
         SELECT $1, $2, unnest($3::TEXT[])",
    )
    .bind(scope.tenant_id())
    .bind(zone)
    .bind(countries)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces a zone's rates.
pub(crate) async fn set_rates(
    conn: &mut PgConnection,
    scope: &impl Scope,
    zone: &str,
    rates: &[TaxRate],
) -> Result<()> {
    sqlx::query("DELETE FROM tax_rates WHERE tenant_id = $1 AND zone_id = $2")
        .bind(scope.tenant_id())
        .bind(zone)
        .execute(&mut *conn)
        .await?;
    for r in rates {
        sqlx::query(
            "INSERT INTO tax_rates (tenant_id, zone_id, tax_class, rate_bps, name) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(scope.tenant_id())
        .bind(zone)
        .bind(&r.tax_class)
        .bind(r.rate_bps)
        .bind(r.name.trim())
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// One zone, or 404.
pub(crate) async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<TaxZone> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT id, name FROM tax_zones WHERE tenant_id = $1 AND id = $2")
            .bind(scope.tenant_id())
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let (id, name) = row.ok_or_else(Error::not_found)?;
    let countries: Vec<(String,)> = sqlx::query_as(
        "SELECT country FROM tax_zone_countries WHERE tenant_id = $1 AND zone_id = $2 \
         ORDER BY country",
    )
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    let rates = sqlx::query_as::<_, TaxRate>(
        "SELECT tax_class, rate_bps, name FROM tax_rates WHERE tenant_id = $1 AND zone_id = $2 \
         ORDER BY tax_class",
    )
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(TaxZone {
        id,
        name,
        countries: countries.into_iter().map(|(c,)| c).collect(),
        rates,
    })
}

/// Creates a zone in the shop's transaction and publishes
/// `tax_zone.created`.
pub(crate) async fn create(
    tx: &mut TenantTx,
    tenant: &Tenant,
    name: &str,
    countries: &[String],
    rates: &[TaxRate],
) -> Result<TaxZone> {
    let id = TaxZoneId::new();
    sqlx::query("INSERT INTO tax_zones (id, tenant_id, name) VALUES ($1, $2, $3)")
        .bind(id.as_str())
        .bind(tenant.id())
        .bind(name.trim())
        .execute(&mut **tx)
        .await?;
    set_countries(tx, tenant, id.as_str(), countries).await?;
    set_rates(tx, tenant, id.as_str(), rates).await?;
    let zone = load(tx, tenant, id.as_str()).await?;
    tenant.publish(tx, "tax_zone.created", &zone).await?;
    Ok(zone)
}

fn parse(id: &str) -> Result<String> {
    TaxZoneId::parse(id)
        .map(|i| i.to_string())
        .map_err(|_| Error::not_found())
}

/// The shop's tax zones, by name (`tax:read`).
#[get("/api/tax/zones", tag = "tax")]
async fn list_zones(db: Db, tenant: Tenant) -> Result<Json<ZoneList>> {
    let mut tx = tenant.begin_for(&db, &TAX_READ).await?;
    let ids: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM tax_zones WHERE tenant_id = $1 ORDER BY name, id")
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

/// Create a tax zone (`tax:manage`). A name the shop already uses, or a
/// country already in another zone, is a 409. Publishes
/// `tax_zone.created`.
#[post("/api/tax/zones", tag = "tax", status = 201)]
async fn create_zone(
    db: Db,
    tenant: Tenant,
    Valid(Json(z)): Valid<Json<NewZone>>,
) -> Result<(StatusCode, Json<TaxZone>)> {
    let countries = countries(&z.countries)?;
    let rates = rates(&z.rates)?;
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let zone = create(&mut tx, &tenant, &z.name, &countries, &rates).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(zone)))
}

/// One tax zone (`tax:read`).
#[get("/api/tax/zones/{id}", tag = "tax")]
async fn get_zone(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<TaxZone>> {
    let id = parse(&id)?;
    let mut tx = tenant.begin_for(&db, &TAX_READ).await?;
    let zone = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(zone))
}

/// Rename a zone or replace its countries or rates (`tax:manage`).
/// Publishes `tax_zone.updated`.
#[patch("/api/tax/zones/{id}", tag = "tax")]
async fn update_zone(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(z)): Valid<Json<ZoneChange>>,
) -> Result<Json<TaxZone>> {
    let id = parse(&id)?;
    let new_countries = z.countries.as_deref().map(countries).transpose()?;
    let new_rates = z.rates.as_deref().map(rates).transpose()?;
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let found = sqlx::query(
        "UPDATE tax_zones SET name = COALESCE($3, name) WHERE tenant_id = $1 AND id = $2",
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
    if let Some(r) = &new_rates {
        set_rates(&mut tx, &tenant, &id, r).await?;
    }
    let zone = load(&mut tx, &tenant, &id).await?;
    tenant.publish(&mut tx, "tax_zone.updated", &zone).await?;
    tx.commit().await?;
    Ok(Json(zone))
}

/// Delete a zone; its countries are no longer taxed (`tax:manage`).
/// Publishes `tax_zone.deleted`.
#[delete("/api/tax/zones/{id}", tag = "tax", status = 204)]
async fn delete_zone(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = parse(&id)?;
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM tax_zones WHERE tenant_id = $1 AND id = $2")
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
            "tax_zone.deleted",
            &serde_json::json!({ "id": id }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
