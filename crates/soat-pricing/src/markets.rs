//! Markets: groups of countries that buy in one currency, such as
//! "Switzerland" in CHF or "Nordics" in SEK. A buyer's country picks the
//! market, the market picks the currency. Price lists in that currency win;
//! otherwise an item priced in the market's base currency is converted at
//! the market's exchange rate and rounded as the market says.

use soat_core::address::country_name;
use soat_core::{convert, id_kind, Currency, Rounding};
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;

use crate::{currency, PRICING_READ, PRICING_WRITE};

id_kind!(MarketId, MarketKind, "mkt");

/// Most countries in one market.
pub const MAX_COUNTRIES: usize = 250;

/// A market.
#[model]
#[derive(sqlx::FromRow)]
pub struct Market {
    /// e.g. `mkt_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// Unique in the shop, e.g. `Switzerland`.
    pub name: String,
    /// ISO 3166 codes, e.g. `["CH", "LI"]`. A country is in at most one
    /// active market.
    pub countries: Vec<String>,
    /// What buyers in the market pay in, e.g. `CHF`.
    pub currency: String,
    /// The currency converted from, e.g. `EUR`; null without a rate.
    pub base_currency: Option<String>,
    /// Units of `currency` per unit of `base_currency`, in millionths:
    /// `950000` is 0.95.
    pub exchange_rate_ppm: Option<i64>,
    /// `exact` (to the cent), `whole` (up to a whole unit) or `charm` (up
    /// to the next `.99`).
    pub rounding: String,
    pub active: bool,
}

/// Body of a create request.
#[model]
pub struct NewMarket {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    pub countries: Vec<String>,
    pub currency: String,
    pub base_currency: Option<String>,
    pub exchange_rate_ppm: Option<i64>,
    pub rounding: Option<String>,
    pub active: Option<bool>,
}

/// Body of an update request: only what is sent changes. Send
/// `exchange_rate_ppm` 0 to stop converting.
#[model]
pub struct MarketChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    pub countries: Option<Vec<String>>,
    pub currency: Option<String>,
    pub base_currency: Option<String>,
    pub exchange_rate_ppm: Option<i64>,
    pub rounding: Option<String>,
    pub active: Option<bool>,
}

const COLUMNS: &str =
    "id, name, countries, currency, base_currency, exchange_rate_ppm, rounding, active";

fn invalid(m: impl Into<String>) -> Error {
    Error::unprocessable(m.into()).with_code("invalid_market")
}

/// Checks a market and puts its codes in canonical form.
fn check(m: &mut Market) -> Result<()> {
    let mut countries: Vec<String> = Vec::new();
    for c in &m.countries {
        let c = c.trim().to_ascii_uppercase();
        if country_name(&c).is_none() {
            return Err(invalid(format!("{c} is not a country code like CH")));
        }
        if !countries.contains(&c) {
            countries.push(c);
        }
    }
    if countries.is_empty() || countries.len() > MAX_COUNTRIES {
        return Err(invalid(format!(
            "a market has 1 to {MAX_COUNTRIES} countries"
        )));
    }
    m.countries = countries;
    currency(&m.currency)?;
    match (&m.base_currency, m.exchange_rate_ppm) {
        (None, None) => {}
        (Some(base), Some(rate)) if rate > 0 => {
            currency(base)?;
            if *base == m.currency {
                return Err(invalid("base_currency is the currency converted from"));
            }
        }
        _ => {
            return Err(invalid(
                "base_currency and a positive exchange_rate_ppm go together",
            ))
        }
    }
    if Rounding::parse(&m.rounding).is_none() {
        return Err(invalid("rounding is exact, whole or charm"));
    }
    Ok(())
}

/// A 409 when another active market has one of the countries.
async fn check_overlap(tx: &mut TenantTx, tenant: &Tenant, m: &Market) -> Result<()> {
    if !m.active {
        return Ok(());
    }
    let other: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM markets WHERE tenant_id = $1 AND id <> $2 AND active \
         AND countries && $3 LIMIT 1",
    )
    .bind(tenant.id())
    .bind(&m.id)
    .bind(&m.countries)
    .fetch_optional(&mut **tx)
    .await?;
    match other {
        Some((name,)) => Err(
            Error::conflict(format!("a country is in the market {name} already"))
                .with_code("country_in_market"),
        ),
        None => Ok(()),
    }
}

async fn load(tx: &mut TenantTx, tenant: &Tenant, id: &str) -> Result<Market> {
    let id = MarketId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Market>(&format!(
        "SELECT {COLUMNS} FROM markets WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(Error::not_found)
}

/// The active market of `country` (e.g. `CH`), if any.
pub async fn market_for(
    conn: &mut PgConnection,
    scope: &impl Scope,
    country: &str,
) -> Result<Option<Market>> {
    Ok(sqlx::query_as::<_, Market>(&format!(
        "SELECT {COLUMNS} FROM markets WHERE tenant_id = $1 AND active \
         AND $2 = ANY(countries) ORDER BY name LIMIT 1"
    ))
    .bind(scope.tenant_id())
    .bind(country)
    .fetch_optional(conn)
    .await?)
}

/// The shop's active markets, by name; for a country picker.
pub async fn active_markets(conn: &mut PgConnection, scope: &impl Scope) -> Result<Vec<Market>> {
    Ok(sqlx::query_as::<_, Market>(&format!(
        "SELECT {COLUMNS} FROM markets WHERE tenant_id = $1 AND active ORDER BY name, id"
    ))
    .bind(scope.tenant_id())
    .fetch_all(conn)
    .await?)
}

/// `minor` units of `from` in `to`, through an active market that converts
/// `from` into `to`: the amount and the market's id.
pub async fn converted(
    conn: &mut PgConnection,
    scope: &impl Scope,
    minor: i64,
    from: Currency,
    to: Currency,
) -> Result<Option<(i64, String)>> {
    let row: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT id, exchange_rate_ppm, rounding FROM markets WHERE tenant_id = $1 AND active \
         AND currency = $2 AND base_currency = $3 ORDER BY name, id LIMIT 1",
    )
    .bind(scope.tenant_id())
    .bind(to.as_str())
    .bind(from.as_str())
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|(id, rate, rounding)| {
        let rounding = Rounding::parse(&rounding).unwrap_or(Rounding::Exact);
        convert(minor, from, to, rate, rounding).map(|m| (m, id))
    }))
}

/// The shop's markets (`pricing:read`). `?currency=CHF` filters.
#[get("/api/markets", tag = "pricing")]
pub(crate) async fn list_markets(
    db: Db,
    tenant: Tenant,
    params: ListParams,
) -> Result<Json<Paged<Market>>> {
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let page = TenantListing::new("markets", COLUMNS)
        .sortable(["name"])
        .search(["name"])
        .filter_text("currency")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Create a market (`pricing:write`): countries, the currency they buy in,
/// and optionally an exchange rate from a base currency with a rounding.
/// A country already in an active market is a 409 `country_in_market`.
/// Publishes `market.created`.
#[post("/api/markets", tag = "pricing", status = 201)]
pub(crate) async fn create_market(
    db: Db,
    tenant: Tenant,
    Valid(Json(b)): Valid<Json<NewMarket>>,
) -> Result<(StatusCode, Json<Market>)> {
    let mut m = Market {
        id: MarketId::new().as_str().to_owned(),
        name: b.name,
        countries: b.countries,
        currency: b.currency,
        base_currency: b.base_currency,
        exchange_rate_ppm: b.exchange_rate_ppm,
        rounding: b.rounding.unwrap_or_else(|| "exact".to_owned()),
        active: b.active.unwrap_or(true),
    };
    check(&mut m)?;
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    check_overlap(&mut tx, &tenant, &m).await?;
    let m = save(&mut tx, &tenant, &m, true).await?;
    tenant.publish(&mut tx, "market.created", &m).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn save(tx: &mut TenantTx, tenant: &Tenant, m: &Market, new: bool) -> Result<Market> {
    let sql = if new {
        format!(
            "INSERT INTO markets (id, tenant_id, name, countries, currency, base_currency, \
             exchange_rate_ppm, rounding, active) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             RETURNING {COLUMNS}"
        )
    } else {
        format!(
            "UPDATE markets SET name = $3, countries = $4, currency = $5, base_currency = $6, \
             exchange_rate_ppm = $7, rounding = $8, active = $9 \
             WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
        )
    };
    let res = sqlx::query_as::<_, Market>(&sql)
        .bind(&m.id)
        .bind(tenant.id())
        .bind(&m.name)
        .bind(&m.countries)
        .bind(&m.currency)
        .bind(&m.base_currency)
        .bind(m.exchange_rate_ppm)
        .bind(&m.rounding)
        .bind(m.active)
        .fetch_one(&mut **tx)
        .await;
    match res {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(Error::conflict("a market has this name already").with_code("name_taken"))
        }
        other => Ok(other?),
    }
}

/// One market (`pricing:read`).
#[get("/api/markets/{id}", tag = "pricing")]
pub(crate) async fn get_market(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<Json<Market>> {
    let mut tx = tenant.begin_for(&db, &PRICING_READ).await?;
    let m = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Change a market (`pricing:write`); `exchange_rate_ppm` 0 stops
/// converting. Publishes `market.updated`.
#[patch("/api/markets/{id}", tag = "pricing")]
pub(crate) async fn update_market(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<MarketChange>>,
) -> Result<Json<Market>> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let mut m = load(&mut tx, &tenant, &id).await?;
    if let Some(v) = c.name {
        m.name = v;
    }
    if let Some(v) = c.countries {
        m.countries = v;
    }
    if let Some(v) = c.currency {
        m.currency = v;
    }
    if let Some(v) = c.base_currency {
        m.base_currency = Some(v);
    }
    match c.exchange_rate_ppm {
        Some(0) => {
            m.exchange_rate_ppm = None;
            m.base_currency = None;
        }
        Some(v) => m.exchange_rate_ppm = Some(v),
        None => {}
    }
    if let Some(v) = c.rounding {
        m.rounding = v;
    }
    if let Some(v) = c.active {
        m.active = v;
    }
    check(&mut m)?;
    check_overlap(&mut tx, &tenant, &m).await?;
    let m = save(&mut tx, &tenant, &m, false).await?;
    tenant.publish(&mut tx, "market.updated", &m).await?;
    tx.commit().await?;
    Ok(Json(m))
}

/// Delete a market (`pricing:write`). Carts and orders keep their
/// currency. Publishes `market.deleted`.
#[delete("/api/markets/{id}", tag = "pricing", status = 204)]
pub(crate) async fn delete_market(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &PRICING_WRITE).await?;
    let m = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM markets WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&m.id)
        .execute(&mut *tx)
        .await?;
    tenant.publish(&mut tx, "market.deleted", &m).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The market for `?country=CH`, for storefronts (public): 404 when the
/// country is in none.
#[get("/api/storefront/markets", tag = "storefront")]
pub(crate) async fn storefront_market(
    db: Db,
    shop: soat_tenancy::Shop,
    params: ListParams,
) -> Result<Json<Market>> {
    let country = params
        .get("country")
        .ok_or_else(|| Error::bad_request("pass ?country=CH").with_code("missing_country"))?
        .to_ascii_uppercase();
    let mut tx = shop.begin(&db).await?;
    let m = market_for(&mut tx, &shop, &country).await?;
    tx.commit().await?;
    m.map(Json).ok_or_else(Error::not_found)
}
