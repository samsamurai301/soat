//! Ready-made zones: EU VAT.

use soat_core::tax::EU_VAT_RATES;
use soat_tenancy::Tenant;
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::zones::{create, TaxRate, ZoneList};
use crate::TAX_MANAGE;

pub(crate) fn mount(app: App) -> App {
    app.mount((eu_vat,))
}

fn rate(class: &str, bps: i64, label: &str) -> TaxRate {
    let pct = if bps % 100 == 0 {
        format!("{}", bps / 100)
    } else {
        format!("{},{:02}", bps / 100, bps % 100)
            .trim_end_matches('0')
            .to_owned()
    };
    TaxRate {
        tax_class: class.to_owned(),
        rate_bps: bps,
        name: format!("{label} {pct}%"),
    }
}

/// Add a zone per EU member state with its standard and main reduced VAT
/// rate, and a `zero` class at 0% (`tax:manage`). Countries already in a
/// zone are skipped, so it is safe to run again. Rates were last reviewed
/// in November 2026: check them. Returns the zones it created, each
/// published as `tax_zone.created`.
#[post("/api/tax/presets/eu-vat", tag = "tax", status = 201)]
async fn eu_vat(db: Db, tenant: Tenant) -> Result<(StatusCode, Json<ZoneList>)> {
    let mut tx = tenant.begin_for(&db, &TAX_MANAGE).await?;
    let taken: Vec<(String,)> =
        sqlx::query_as("SELECT country FROM tax_zone_countries WHERE tenant_id = $1")
            .bind(tenant.id())
            .fetch_all(&mut *tx)
            .await?;
    let mut items = Vec::new();
    for vat in EU_VAT_RATES {
        if taken.iter().any(|(c,)| c == vat.country) {
            continue;
        }
        let mut rates = vec![rate("standard", vat.standard_bps, "VAT")];
        if let Some(r) = vat.reduced_bps {
            rates.push(rate("reduced", r, "VAT"));
        }
        rates.push(rate("zero", 0, "VAT"));
        let name = format!("EU VAT {}", vat.country);
        let zone = create(&mut tx, &tenant, &name, &[vat.country.to_owned()], &rates).await?;
        items.push(zone);
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ZoneList { items })))
}
