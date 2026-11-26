//! The printable page of an invoice or credit note.

use serde_json::{json, Value};
use soat_core::template::render;
use soat_core::Currency;
use soat_orders::PostalAddress;
use soat_tenancy::Scope;
use zapline::db::sqlx::PgConnection;
use zapline::prelude::*;

use crate::{settings, Invoice, Party};

const PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>{{title}} {{number}}</title>
<style>
body{font:14px/1.45 system-ui,sans-serif;color:#111;max-width:800px;margin:2rem auto;padding:0 1rem}
h1{font-size:1.6rem;margin:0 0 .25rem}
.parties{display:flex;justify-content:space-between;gap:2rem;margin:2rem 0}
table{width:100%;border-collapse:collapse;margin:1rem 0}
th,td{padding:.4rem;border-bottom:1px solid #ddd;text-align:left;vertical-align:top}
.n{text-align:right;white-space:nowrap}
.total td{font-weight:700;border-top:2px solid #111}
.muted{color:#555}
@media print{body{margin:0}}
</style></head>
<body>
<h1>{{title}} {{number}}</h1>
<div class="muted">Issued {{issued_on}} · Order {{order_number}}{{#if credits}} · Credits invoice {{credits}}{{/if}}</div>
<div class="parties">
<div><strong>{{seller.name}}</strong><br>{{#each seller.lines}}{{this}}<br>{{/each}}{{#if seller.vat_number}}VAT ID {{seller.vat_number}}<br>{{/if}}{{#if seller.tax_number}}Tax number {{seller.tax_number}}<br>{{/if}}{{#if seller.email}}{{seller.email}}{{/if}}</div>
<div><strong>{{buyer.name}}</strong><br>{{#each buyer.lines}}{{this}}<br>{{/each}}{{#if buyer.vat_number}}VAT ID {{buyer.vat_number}}<br>{{/if}}{{#if buyer.email}}{{buyer.email}}{{/if}}</div>
</div>
<table>
<thead><tr><th>Description</th><th class="n">Qty</th><th class="n">Unit price</th><th class="n">Discount</th><th class="n">VAT</th><th class="n">{{amount_label}}</th></tr></thead>
<tbody>{{#each lines}}<tr><td>{{description}}{{#if sku}} <span class="muted">({{sku}})</span>{{/if}}</td><td class="n">{{quantity}}</td><td class="n">{{unit_price}}</td><td class="n">{{discount}}</td><td class="n">{{rate}}</td><td class="n">{{amount}}</td></tr>{{/each}}</tbody>
</table>
<table>
<thead><tr><th>VAT rate</th><th class="n">Net</th><th class="n">VAT</th><th class="n">Gross</th></tr></thead>
<tbody>{{#each rates}}<tr><td>{{rate}}</td><td class="n">{{net}}</td><td class="n">{{tax}}</td><td class="n">{{gross}}</td></tr>{{/each}}
<tr class="total"><td>Total</td><td class="n">{{net}}</td><td class="n">{{tax}}</td><td class="n">{{total}}</td></tr></tbody>
</table>
{{#if note}}<p>{{note}}</p>{{/if}}
{{#if bank_details}}<p class="muted">{{bank_details}}</p>{{/if}}
{{#if footer}}<p class="muted">{{footer}}</p>{{/if}}
</body></html>
"#;

/// `1900` as `19%`, `550` as `5.5%`.
fn rate(bps: i64) -> String {
    let whole = bps / 100;
    let rest = bps % 100;
    if rest == 0 {
        format!("{whole}%")
    } else if rest % 10 == 0 {
        format!("{whole}.{}%", rest / 10)
    } else {
        format!("{whole}.{rest:02}%")
    }
}

fn address_lines(a: &PostalAddress) -> Vec<String> {
    let name = format!("{} {}", a.first_name, a.last_name)
        .trim()
        .to_owned();
    let town = format!("{} {}", a.postal_code, a.city).trim().to_owned();
    [
        name,
        a.line1.clone(),
        a.line2.clone(),
        town,
        a.region.clone(),
        a.country.clone(),
    ]
    .into_iter()
    .filter(|l| !l.trim().is_empty())
    .collect()
}

fn party(p: &Party) -> Value {
    let mut lines = p.address.as_ref().map(address_lines).unwrap_or_default();
    // The name heads the block already.
    if lines.first() == Some(&p.name) {
        lines.remove(0);
    }
    json!({
        "name": p.name,
        "lines": lines,
        "vat_number": p.vat_number,
        "tax_number": p.tax_number,
        "email": p.email,
    })
}

/// The document's HTML.
pub async fn page(conn: &mut PgConnection, scope: &impl Scope, inv: &Invoice) -> Result<String> {
    let s = settings(conn, scope).await?;
    let credits = match &inv.credits_id {
        Some(id) => crate::get(conn, scope, id).await?.map(|i| i.number),
        None => None,
    };
    let currency = Currency::parse(&inv.currency).ok();
    let money = |minor: i64| match currency {
        Some(c) => c.format(minor),
        None => format!("{minor} {}", inv.currency),
    };
    let inclusive = inv.prices_include_tax;
    let lines: Vec<Value> = inv
        .lines
        .iter()
        .map(|l| {
            json!({
                "description": l.description,
                "sku": l.sku,
                "quantity": l.quantity,
                "unit_price": money(l.unit_price_minor),
                "discount": if l.discount_minor == 0 { String::new() } else { money(-l.discount_minor) },
                "rate": rate(l.rate_bps),
                "amount": money(if inclusive { l.gross_minor } else { l.net_minor }),
            })
        })
        .collect();
    let rates: Vec<Value> = inv
        .rates
        .iter()
        .map(|r| {
            json!({
                "rate": rate(r.rate_bps),
                "net": money(r.net_minor),
                "tax": money(r.tax_minor),
                "gross": money(r.gross_minor),
            })
        })
        .collect();
    let data = json!({
        "title": if inv.kind == "invoice" { "Invoice" } else { "Credit note" },
        "number": inv.number,
        "issued_on": inv.issued_at.get(..10).unwrap_or(&inv.issued_at),
        "order_number": inv.order_number,
        "credits": credits,
        "seller": party(&inv.seller),
        "buyer": party(&inv.buyer),
        "amount_label": if inclusive { "Amount (gross)" } else { "Amount (net)" },
        "lines": lines,
        "rates": rates,
        "net": money(inv.net_minor),
        "tax": money(inv.tax_minor),
        "total": money(inv.total_minor),
        "note": inv.note,
        "bank_details": s.as_ref().map(|s| s.bank_details.clone()),
        "footer": s.as_ref().map(|s| s.footer.clone()),
    });
    render(PAGE, &data, true).map_err(|e| Error::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::rate;

    #[test]
    fn rates() {
        assert_eq!(rate(1900), "19%");
        assert_eq!(rate(550), "5.5%");
        assert_eq!(rate(0), "0%");
        assert_eq!(rate(1025), "10.25%");
    }
}
