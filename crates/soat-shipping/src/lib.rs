//! Shipping.
//!
//! A shop groups the countries it ships to into zones; each zone has
//! rates: a name (`Standard`, `Express`), a price in a currency, and
//! optional inclusive bounds on the order's value and weight, so "free
//! from 50 EUR" is a rate at 0 with `min_subtotal_minor` 5000. [`options`]
//! lists the rates a buyer can choose, cheapest first; checkout (C9)
//! offers them and checks the choice with [`option`]. A country in no zone
//! cannot be shipped to. [`carriers`] print labels.

pub mod carriers;
mod zones;

use std::sync::Arc;

use carriers::{CarrierProvider, Carriers};
use soat_core::secret::Sealer;

use soat_core::tax::is_tax_class;
use soat_core::{id_kind, Currency, Permission};
use soat_module::Module;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

pub use zones::{ShippingZone, ShippingZoneId};

id_kind!(ShippingRateId, ShippingRateKind, "shr");

/// See shipping zones and rates.
pub const SHIPPING_READ: Permission =
    Permission::new("shipping:read", "See shipping zones and rates").for_staff();
/// Change shipping zones and rates. Not given to staff by default.
pub const SHIPPING_MANAGE: Permission =
    Permission::new("shipping:manage", "Change shipping zones and rates");

/// The shipping module, with the carrier adapters of this install.
#[derive(Default)]
pub struct Shipping {
    carriers: Vec<Arc<dyn CarrierProvider>>,
    sealer: Option<Sealer>,
}

impl Shipping {
    /// With no carrier adapters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a carrier adapter; one with the same name replaces it.
    pub fn with(mut self, carrier: impl CarrierProvider + 'static) -> Self {
        self.carriers.retain(|c| c.name() != carrier.name());
        self.carriers.push(Arc::new(carrier));
        self
    }

    /// Seals carrier credentials with `sealer`; without one, shops cannot
    /// add carrier accounts.
    pub fn with_sealer(mut self, sealer: Option<Sealer>) -> Self {
        self.sealer = sealer;
        self
    }
}

impl Module for Shipping {
    fn name(&self) -> &'static str {
        "shipping"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[SHIPPING_READ, SHIPPING_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.state(Carriers::new(self.carriers.clone(), self.sealer.clone()))
            .plugin(zones::mount)
            .plugin(carriers::mount)
            .mount((create_rate, replace_rate, delete_rate, options_route))
    }
}

/// A shipping rate.
#[model]
#[derive(sqlx::FromRow)]
pub struct ShippingRate {
    /// e.g. `shr_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub zone_id: String,
    /// What the buyer sees, e.g. `Standard` or `Express`.
    pub name: String,
    /// e.g. `dhl`; empty when not tied to one.
    pub carrier: String,
    /// ISO 4217; the rate is only offered for orders in it.
    pub currency: String,
    pub price_minor: i64,
    pub min_subtotal_minor: Option<i64>,
    pub max_subtotal_minor: Option<i64>,
    pub min_weight_grams: Option<i64>,
    pub max_weight_grams: Option<i64>,
    /// Delivery estimate in days.
    pub min_days: Option<i32>,
    pub max_days: Option<i32>,
    /// How the shipping charge is taxed. Default `standard`.
    pub tax_class: String,
    pub active: bool,
}

/// Body of creating or replacing a rate. Bounds are inclusive; leave one
/// out for no bound.
#[model]
pub struct RateInput {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[validate(length(max = 50))]
    pub carrier: Option<String>,
    pub currency: String,
    #[validate(range(min = 0))]
    pub price_minor: i64,
    #[validate(range(min = 0))]
    pub min_subtotal_minor: Option<i64>,
    #[validate(range(min = 0))]
    pub max_subtotal_minor: Option<i64>,
    #[validate(range(min = 0))]
    pub min_weight_grams: Option<i64>,
    #[validate(range(min = 0))]
    pub max_weight_grams: Option<i64>,
    #[validate(range(min = 0, max = 365))]
    pub min_days: Option<i32>,
    #[validate(range(min = 0, max = 365))]
    pub max_days: Option<i32>,
    pub tax_class: Option<String>,
    pub active: Option<bool>,
}

pub(crate) const RATE_COLUMNS: &str = "id, zone_id, name, carrier, currency, price_minor, \
     min_subtotal_minor, max_subtotal_minor, min_weight_grams, max_weight_grams, \
     min_days, max_days, tax_class, active";

fn bounds(name: &str, min: Option<i64>, max: Option<i64>) -> Result<()> {
    match (min, max) {
        (Some(a), Some(b)) if a > b => Err(Error::unprocessable(format!(
            "min_{name} is above max_{name}"
        ))
        .with_code("invalid_bounds")),
        _ => Ok(()),
    }
}

fn checked(r: &RateInput) -> Result<(Currency, String)> {
    let currency = Currency::parse(&r.currency)
        .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?;
    bounds("subtotal_minor", r.min_subtotal_minor, r.max_subtotal_minor)?;
    bounds("weight_grams", r.min_weight_grams, r.max_weight_grams)?;
    bounds("days", r.min_days.map(i64::from), r.max_days.map(i64::from))?;
    let class = r.tax_class.clone().unwrap_or_else(|| "standard".to_owned());
    if !is_tax_class(&class) {
        return Err(Error::unprocessable(format!("{class} is not a tax class"))
            .with_code("invalid_tax_class"));
    }
    Ok((currency, class))
}

/// Add a rate to a zone (`shipping:manage`). Publishes
/// `shipping_rate.created`.
#[post("/api/shipping/zones/{id}/rates", tag = "shipping", status = 201)]
async fn create_rate(
    db: Db,
    tenant: Tenant,
    Path(zone): Path<String>,
    Valid(Json(r)): Valid<Json<RateInput>>,
) -> Result<(StatusCode, Json<ShippingRate>)> {
    let zone = ShippingZoneId::parse(&zone).map_err(|_| Error::not_found())?;
    let (currency, class) = checked(&r)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM shipping_zones WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.id())
            .bind(zone.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    exists.ok_or_else(Error::not_found)?;
    let rate = sqlx::query_as::<_, ShippingRate>(&format!(
        "INSERT INTO shipping_rates (id, tenant_id, zone_id, name, carrier, currency, \
         price_minor, min_subtotal_minor, max_subtotal_minor, min_weight_grams, \
         max_weight_grams, min_days, max_days, tax_class, active) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         RETURNING {RATE_COLUMNS}"
    ))
    .bind(ShippingRateId::new().as_str())
    .bind(tenant.id())
    .bind(zone.as_str())
    .bind(r.name.trim())
    .bind(r.carrier.as_deref().unwrap_or("").trim())
    .bind(currency.as_str())
    .bind(r.price_minor)
    .bind(r.min_subtotal_minor)
    .bind(r.max_subtotal_minor)
    .bind(r.min_weight_grams)
    .bind(r.max_weight_grams)
    .bind(r.min_days)
    .bind(r.max_days)
    .bind(&class)
    .bind(r.active.unwrap_or(true))
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "shipping_rate.created", &rate)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(rate)))
}

/// Replace a rate (`shipping:manage`). Publishes `shipping_rate.updated`.
#[put("/api/shipping/rates/{id}", tag = "shipping")]
async fn replace_rate(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(r)): Valid<Json<RateInput>>,
) -> Result<Json<ShippingRate>> {
    let id = ShippingRateId::parse(&id).map_err(|_| Error::not_found())?;
    let (currency, class) = checked(&r)?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let rate = sqlx::query_as::<_, ShippingRate>(&format!(
        "UPDATE shipping_rates SET name = $3, carrier = $4, currency = $5, price_minor = $6, \
         min_subtotal_minor = $7, max_subtotal_minor = $8, min_weight_grams = $9, \
         max_weight_grams = $10, min_days = $11, max_days = $12, tax_class = $13, \
         active = $14 WHERE tenant_id = $1 AND id = $2 RETURNING {RATE_COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .bind(r.name.trim())
    .bind(r.carrier.as_deref().unwrap_or("").trim())
    .bind(currency.as_str())
    .bind(r.price_minor)
    .bind(r.min_subtotal_minor)
    .bind(r.max_subtotal_minor)
    .bind(r.min_weight_grams)
    .bind(r.max_weight_grams)
    .bind(r.min_days)
    .bind(r.max_days)
    .bind(&class)
    .bind(r.active.unwrap_or(true))
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Error::not_found)?;
    tenant
        .publish(&mut tx, "shipping_rate.updated", &rate)
        .await?;
    tx.commit().await?;
    Ok(Json(rate))
}

/// Delete a rate (`shipping:manage`). Publishes `shipping_rate.deleted`.
#[delete("/api/shipping/rates/{id}", tag = "shipping", status = 204)]
async fn delete_rate(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let id = ShippingRateId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &SHIPPING_MANAGE).await?;
    let gone = sqlx::query("DELETE FROM shipping_rates WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    tenant
        .publish(
            &mut tx,
            "shipping_rate.deleted",
            &serde_json::json!({ "id": id.as_str() }),
        )
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What is being shipped where.
#[model]
pub struct ShippingQuery {
    /// e.g. `FR`.
    pub country: String,
    /// The order's currency.
    pub currency: String,
    /// The order's value before shipping.
    pub subtotal_minor: i64,
    pub weight_grams: i64,
}

/// A rate a buyer can choose.
#[model]
pub struct ShippingOption {
    pub rate_id: String,
    pub name: String,
    pub carrier: String,
    pub price_minor: i64,
    pub currency: String,
    pub min_days: Option<i32>,
    pub max_days: Option<i32>,
    pub tax_class: String,
}

/// Options for a query.
#[model]
pub struct ShippingOptions {
    pub items: Vec<ShippingOption>,
}

const MATCH: &str = "FROM shipping_rates r \
     JOIN shipping_zone_countries c ON c.zone_id = r.zone_id AND c.tenant_id = r.tenant_id \
     WHERE r.tenant_id = $1 AND c.country = $2 AND r.currency = $3 AND r.active \
     AND (r.min_subtotal_minor IS NULL OR r.min_subtotal_minor <= $4) \
     AND (r.max_subtotal_minor IS NULL OR r.max_subtotal_minor >= $4) \
     AND (r.min_weight_grams IS NULL OR r.min_weight_grams <= $5) \
     AND (r.max_weight_grams IS NULL OR r.max_weight_grams >= $5)";

fn to_option(r: ShippingRate) -> ShippingOption {
    ShippingOption {
        rate_id: r.id,
        name: r.name,
        carrier: r.carrier,
        price_minor: r.price_minor,
        currency: r.currency,
        min_days: r.min_days,
        max_days: r.max_days,
        tax_class: r.tax_class,
    }
}

/// The rates that apply to `q`, cheapest first. Run it in the shop's
/// transaction.
pub async fn options(
    conn: &mut PgConnection,
    scope: &impl Scope,
    q: &ShippingQuery,
) -> Result<Vec<ShippingOption>> {
    let country = q.country.trim().to_ascii_uppercase();
    let rows = sqlx::query_as::<_, ShippingRate>(&format!(
        "SELECT {} {MATCH} ORDER BY r.price_minor, r.name, r.id",
        prefixed()
    ))
    .bind(scope.tenant_id())
    .bind(&country)
    .bind(q.currency.trim().to_ascii_uppercase())
    .bind(q.subtotal_minor)
    .bind(q.weight_grams)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(to_option).collect())
}

/// The countries the shop ships to (with an active rate), sorted. Run it
/// in the shop's transaction.
pub async fn countries(conn: &mut PgConnection, scope: &impl Scope) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT c.country FROM shipping_zone_countries c \
         JOIN shipping_rates r ON r.zone_id = c.zone_id AND r.tenant_id = c.tenant_id \
         WHERE c.tenant_id = $1 AND r.active ORDER BY c.country",
    )
    .bind(scope.tenant_id())
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(c,)| c).collect())
}

/// The rate `rate_id` if it applies to `q`, else `None`. Run it in the
/// shop's transaction.
pub async fn option(
    conn: &mut PgConnection,
    scope: &impl Scope,
    rate_id: &str,
    q: &ShippingQuery,
) -> Result<Option<ShippingOption>> {
    let country = q.country.trim().to_ascii_uppercase();
    let row =
        sqlx::query_as::<_, ShippingRate>(&format!("SELECT {} {MATCH} AND r.id = $6", prefixed()))
            .bind(scope.tenant_id())
            .bind(&country)
            .bind(q.currency.trim().to_ascii_uppercase())
            .bind(q.subtotal_minor)
            .bind(q.weight_grams)
            .bind(rate_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(to_option))
}

fn prefixed() -> String {
    RATE_COLUMNS
        .split(',')
        .map(|c| format!("r.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rates a buyer could choose for an order (`shipping:read`), as
/// checkout offers them.
#[post("/api/shipping/options", tag = "shipping")]
async fn options_route(
    db: Db,
    tenant: Tenant,
    Json(q): Json<ShippingQuery>,
) -> Result<Json<ShippingOptions>> {
    let mut tx = tenant.begin_for(&db, &SHIPPING_READ).await?;
    let items = options(&mut tx, &tenant, &q).await?;
    tx.commit().await?;
    Ok(Json(ShippingOptions { items }))
}
