//! Tax.
//!
//! A shop groups countries into tax zones; each zone has a rate per tax
//! class (`standard`, `reduced`, or the shop's own). Products carry a tax
//! class; the class `exempt` is never taxed, and a class a zone does not
//! rate falls back to the zone's `standard` rate. A country in no zone is
//! not taxed (exports).
//!
//! EU VAT, the default case: [`presets`] fills a zone per member state.
//! Inside the EU the shop charges its origin country's VAT, or, with
//! `charge_destination_vat` (One-Stop Shop), the buyer's. A business buyer
//! in another member state with a VAT number pays none (reverse charge).
//!
//! When prices include tax, the gross price stays the same in every
//! country and the tax is taken out of it; otherwise tax is added on top.
//! [`quote`] does the sums; checkout (C9) calls it. Tax providers come in
//! C5.

mod presets;
mod zones;

use soat_core::tax::{self, is_eu, tax_on, vat_number_country, EXEMPT, STANDARD};
use soat_core::Permission;
use soat_module::Module;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;

pub use zones::TaxZoneId;

/// Most lines in one quote.
pub const MAX_QUOTE_LINES: usize = 500;

/// See tax settings and zones, and quote tax.
pub const TAX_READ: Permission =
    Permission::new("tax:read", "See tax settings, zones and rates").for_staff();
/// Change tax settings, zones and rates. Not given to staff by default.
pub const TAX_MANAGE: Permission =
    Permission::new("tax:manage", "Change tax settings, zones and rates");

/// The tax module.
pub struct Taxes;

impl Module for Taxes {
    fn name(&self) -> &'static str {
        "tax"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[TAX_READ, TAX_MANAGE]
    }

    fn mount(&self, app: App) -> App {
        app.mount((get_settings, put_settings, quote_route))
            .plugin(zones::mount)
            .plugin(presets::mount)
    }
}

/// How the shop charges tax.
#[model]
#[derive(sqlx::FromRow)]
pub struct TaxSettings {
    /// Where the shop is registered for tax, e.g. `DE`.
    pub origin_country: Option<String>,
    /// Whether prices hold tax (usual for EU consumer shops). Default true.
    pub prices_include_tax: bool,
    /// EU One-Stop Shop: charge the buyer's country's VAT on sales to other
    /// member states instead of the origin's. Default false.
    pub charge_destination_vat: bool,
}

impl Default for TaxSettings {
    fn default() -> Self {
        Self {
            origin_country: None,
            prices_include_tax: true,
            charge_destination_vat: false,
        }
    }
}

/// The shop's tax settings; defaults until set. Run it in the shop's
/// transaction.
pub async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<TaxSettings> {
    let row = sqlx::query_as::<_, TaxSettings>(
        "SELECT origin_country, prices_include_tax, charge_destination_vat \
         FROM tax_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.unwrap_or_default())
}

fn country(raw: &str) -> Result<String> {
    let c = raw.trim().to_ascii_uppercase();
    if soat_core::address::is_country_code(&c) {
        Ok(c)
    } else {
        Err(
            Error::unprocessable(format!("{raw} is not an ISO 3166-1 alpha-2 country code"))
                .with_code("invalid_country"),
        )
    }
}

/// The shop's tax settings (`tax:read`).
#[get("/api/tax/settings", tag = "tax")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<TaxSettings>> {
    let mut tx = tenant.begin_for(&db, &TAX_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    Ok(Json(s))
}

/// Replace the shop's tax settings (`tax:manage`). Publishes
/// `tax.settings_updated`.
#[put("/api/tax/settings", tag = "tax")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Json(s): Json<TaxSettings>,
) -> Result<Json<TaxSettings>> {
    let origin = s.origin_country.as_deref().map(country).transpose()?;
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let saved = sqlx::query_as::<_, TaxSettings>(
        "INSERT INTO tax_settings \
         (tenant_id, origin_country, prices_include_tax, charge_destination_vat) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id) DO UPDATE SET \
         origin_country = EXCLUDED.origin_country, \
         prices_include_tax = EXCLUDED.prices_include_tax, \
         charge_destination_vat = EXCLUDED.charge_destination_vat \
         RETURNING origin_country, prices_include_tax, charge_destination_vat",
    )
    .bind(tenant.id())
    .bind(origin)
    .bind(s.prices_include_tax)
    .bind(s.charge_destination_vat)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "tax.settings_updated", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// What to quote: where the goods go, and amounts per tax class.
#[derive(Debug, Clone)]
pub struct QuoteInput {
    /// The buyer's (shipping) country.
    pub country: String,
    /// A business buyer's VAT number, for the reverse charge.
    pub vat_number: Option<String>,
    /// `(tax_class, amount_minor)`: line totals after discounts, gross or
    /// net as the shop's prices are.
    pub lines: Vec<(String, i64)>,
}

/// Tax on a set of amounts.
#[model]
pub struct TaxQuote {
    /// The country whose rates apply (the origin's or the buyer's).
    pub tax_country: String,
    /// The zone that rated it; null when the country is in none (no tax).
    pub zone_id: Option<String>,
    pub prices_include_tax: bool,
    /// A business buyer in another EU state accounts for the VAT.
    pub reverse_charge: bool,
    pub lines: Vec<TaxLine>,
    /// Sum of the lines' tax.
    pub tax_minor: i64,
}

/// Tax on one amount.
#[model]
pub struct TaxLine {
    pub tax_class: String,
    pub amount_minor: i64,
    /// Basis points: 1900 is 19%.
    pub rate_bps: i64,
    pub tax_minor: i64,
}

/// Works out the tax on `input` with the shop's settings and zones. A VAT
/// number that is not shaped like an EU one is a 422. Run it in the
/// shop's transaction.
pub async fn quote(
    conn: &mut PgConnection,
    scope: &impl Scope,
    input: &QuoteInput,
) -> Result<TaxQuote> {
    let settings = settings(conn, scope).await?;
    let destination = country(&input.country)?;
    let origin = settings.origin_country.as_deref();
    let reverse_charge = match input.vat_number.as_deref().map(str::trim) {
        None | Some("") => false,
        Some(vat) => {
            let vat_country = vat_number_country(vat).ok_or_else(|| {
                Error::unprocessable(format!("{vat} is not an EU VAT number"))
                    .with_code("invalid_vat_number")
            })?;
            // The buyer is registered where the goods go, in another state.
            vat_country == destination && origin.is_some_and(|o| is_eu(o) && o != destination)
        }
    };
    let tax_country = match origin {
        Some(o) if is_eu(o) && is_eu(&destination) && !settings.charge_destination_vat => {
            o.to_owned()
        }
        _ => destination,
    };
    let zone: Option<(String,)> = sqlx::query_as(
        "SELECT zone_id FROM tax_zone_countries WHERE tenant_id = $1 AND country = $2",
    )
    .bind(scope.tenant_id())
    .bind(&tax_country)
    .fetch_optional(&mut *conn)
    .await?;
    let zone_id = zone.map(|(z,)| z);
    let rates: Vec<(String, i64)> =
        match &zone_id {
            Some(z) if !reverse_charge => sqlx::query_as(
                "SELECT tax_class, rate_bps FROM tax_rates WHERE tenant_id = $1 AND zone_id = $2",
            )
            .bind(scope.tenant_id())
            .bind(z)
            .fetch_all(&mut *conn)
            .await?,
            _ => Vec::new(),
        };
    let rate_of = |class: &str| -> i64 {
        if class == EXEMPT {
            return 0;
        }
        let find = |c: &str| rates.iter().find(|(k, _)| k == c).map(|(_, r)| *r);
        find(class).or_else(|| find(STANDARD)).unwrap_or(0)
    };
    let mut lines = Vec::with_capacity(input.lines.len());
    let mut total = 0i64;
    for (class, amount) in &input.lines {
        let rate_bps = rate_of(class);
        let tax_minor = tax_on(*amount, rate_bps, settings.prices_include_tax);
        total = total
            .checked_add(tax_minor)
            .ok_or_else(|| Error::unprocessable("tax total too large"))?;
        lines.push(TaxLine {
            tax_class: class.clone(),
            amount_minor: *amount,
            rate_bps,
            tax_minor,
        });
    }
    Ok(TaxQuote {
        tax_country,
        zone_id,
        prices_include_tax: settings.prices_include_tax,
        reverse_charge,
        lines,
        tax_minor: total,
    })
}

/// Body of a tax quote.
#[model]
pub struct QuoteRequest {
    /// The buyer's country, e.g. `FR`.
    pub country: String,
    /// A business buyer's VAT number.
    pub vat_number: Option<String>,
    pub lines: Vec<QuoteRequestLine>,
}

/// One amount to tax: give an `item_id` to use its product's tax class,
/// or a `tax_class` (default `standard`).
#[model]
pub struct QuoteRequestLine {
    pub item_id: Option<String>,
    pub tax_class: Option<String>,
    pub amount_minor: i64,
}

/// Work out the tax on some amounts for a buyer's country (`tax:read`),
/// as checkout will. An unknown item is a 422.
#[post("/api/tax/quote", tag = "tax")]
async fn quote_route(
    db: Db,
    tenant: Tenant,
    Json(q): Json<QuoteRequest>,
) -> Result<Json<TaxQuote>> {
    if q.lines.len() > MAX_QUOTE_LINES {
        return Err(
            Error::unprocessable(format!("at most {MAX_QUOTE_LINES} lines"))
                .with_code("too_many_lines"),
        );
    }
    let mut tx = tenant.begin_for(&db, &TAX_READ).await?;
    let mut lines = Vec::with_capacity(q.lines.len());
    for l in &q.lines {
        let class = match (&l.item_id, &l.tax_class) {
            (Some(item), _) => {
                soat_catalog::stock_item(&mut tx, &tenant, item)
                    .await?
                    .ok_or_else(|| {
                        Error::unprocessable(format!("no item {item}")).with_code("unknown_item")
                    })?
                    .tax_class
            }
            (None, Some(c)) if tax::is_tax_class(c) => c.clone(),
            (None, Some(c)) => {
                return Err(Error::unprocessable(format!("{c} is not a tax class"))
                    .with_code("invalid_tax_class"))
            }
            (None, None) => STANDARD.to_owned(),
        };
        lines.push((class, l.amount_minor));
    }
    let input = QuoteInput {
        country: q.country,
        vat_number: q.vat_number,
        lines,
    };
    let out = quote(&mut tx, &tenant, &input).await?;
    tx.commit().await?;
    Ok(Json(out))
}
