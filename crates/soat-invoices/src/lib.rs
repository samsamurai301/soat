//! Invoices and credit notes.
//!
//! Once a shop gives its legal details, each order gets an invoice: when
//! it is paid (or placed, or only when staff issue it, as the shop
//! chooses). An invoice copies in the seller and buyer, every line with
//! net, tax and gross, the totals per tax rate and a reverse-charge note
//! where it applies, and takes the next number of its series and year
//! (`INV-2026-00001`), with no gaps. Issued invoices never change; each
//! refund of an invoiced order issues a credit note against it, shared
//! over its tax rates, and staff can issue more. Staff and the buyer
//! (holding the order's token) can open a printable document of each.

mod document;
mod einvoice;

use std::sync::Arc;

use soat_core::invoice::{breakdown, credit_split, number, total, Amounts, RateTotal};
use soat_core::{id_kind, Permission};

pub use einvoice::{EInvoiceCheck, EInvoiceProblem, EInvoiceQuery};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_orders::{Order, OrderToken, PostalAddress};
use soat_tenancy::{Scope, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

id_kind!(InvoiceId, InvoiceKind, "inv");

/// See invoices, credit notes and the invoice settings.
pub const INVOICES_READ: Permission =
    Permission::new("invoices:read", "See invoices and credit notes").for_staff();
/// Issue invoices and credit notes, change the invoice settings.
pub const INVOICES_WRITE: Permission = Permission::new(
    "invoices:write",
    "Issue invoices and credit notes, change invoice settings",
)
.for_staff();

/// The note on reverse-charge invoices.
pub const REVERSE_CHARGE_NOTE: &str =
    "Reverse charge: VAT is due from the recipient (Article 196, Directive 2006/112/EC).";

/// The invoices module.
pub struct Invoices;

impl Module for Invoices {
    fn name(&self) -> &'static str {
        "invoices"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "payments", "orders"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[INVOICES_READ, INVOICES_WRITE]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Invoicing)]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[
            soat_module::PersonalData {
                table: "invoices",
                matches: "order_id IN (SELECT id FROM orders WHERE tenant_id = $1 AND (customer_id = $2 OR lower(email) = $3))",
                export: &["number", "kind", "order_number", "issued_at", "currency", "buyer", "total_minor"],
                erase: soat_module::Erase::Keep(
                    "invoices are kept for ten years for tax law (e.g. § 147 AO, § 14b UStG)",
                ),
            },
        ]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            get_settings,
            put_settings,
            list_invoices,
            get_invoice,
            invoice_document,
            issue_for_order,
            issue_credit_note,
            buyer_invoices,
            buyer_document,
            invoice_xml,
            check_invoice_xml,
            buyer_invoice_xml,
        ))
    }
}

/// An issued invoice or credit note and the e-invoice document it makes
/// (see `soat_core::einvoice`), for modules that send it on, such as over
/// Peppol; `None` for an id the shop does not have.
pub async fn e_invoice(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
    buyer_reference: Option<&str>,
) -> Result<Option<(Invoice, soat_core::einvoice::Document)>> {
    let Some(inv) = get(conn, scope, id).await? else {
        return Ok(None);
    };
    let doc = einvoice::document(conn, scope, &inv, buyer_reference).await?;
    Ok(Some((inv, doc)))
}

/// Who issues the shop's invoices, and how.
#[model]
#[derive(sqlx::FromRow)]
pub struct InvoiceSettings {
    pub legal_name: String,
    #[sqlx(json)]
    pub address: PostalAddress,
    pub vat_number: Option<String>,
    /// A national tax number, where one is shown (e.g. Steuernummer).
    pub tax_number: Option<String>,
    pub email: Option<String>,
    /// Printed for buyers who pay by transfer.
    pub bank_details: String,
    /// Printed at the bottom, e.g. the company register entry.
    pub footer: String,
    pub invoice_prefix: String,
    pub credit_note_prefix: String,
    /// `paid` (default), `placed` or `manual`.
    pub issue_on: String,
}

/// Body of setting the invoice details.
#[model]
pub struct SettingsInput {
    #[validate(length(min = 1, max = 200))]
    pub legal_name: String,
    #[validate(nested)]
    pub address: PostalAddress,
    #[validate(length(max = 20))]
    pub vat_number: Option<String>,
    #[validate(length(max = 40))]
    pub tax_number: Option<String>,
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
    #[validate(length(max = 500))]
    #[serde(default)]
    pub bank_details: String,
    #[validate(length(max = 1000))]
    #[serde(default)]
    pub footer: String,
    #[validate(length(max = 20))]
    pub invoice_prefix: Option<String>,
    #[validate(length(max = 20))]
    pub credit_note_prefix: Option<String>,
    pub issue_on: Option<String>,
}

/// One side of an invoice.
#[model]
pub struct Party {
    pub name: String,
    pub address: Option<PostalAddress>,
    pub vat_number: Option<String>,
    pub tax_number: Option<String>,
    pub email: Option<String>,
}

/// A line of an invoice.
#[model]
pub struct InvoiceLine {
    pub description: String,
    pub sku: Option<String>,
    pub quantity: i64,
    /// Per unit, before discounts: gross when prices include tax, else net.
    pub unit_price_minor: i64,
    pub discount_minor: i64,
    pub rate_bps: i64,
    pub net_minor: i64,
    pub tax_minor: i64,
    pub gross_minor: i64,
}

/// Totals at one tax rate.
#[model]
pub struct RateLine {
    pub rate_bps: i64,
    pub net_minor: i64,
    pub tax_minor: i64,
    pub gross_minor: i64,
}

impl From<RateTotal> for RateLine {
    fn from(r: RateTotal) -> Self {
        Self {
            rate_bps: r.rate_bps,
            net_minor: r.amounts.net_minor,
            tax_minor: r.amounts.tax_minor,
            gross_minor: r.amounts.gross_minor,
        }
    }
}

impl From<&RateLine> for RateTotal {
    fn from(r: &RateLine) -> Self {
        Self {
            rate_bps: r.rate_bps,
            amounts: Amounts {
                net_minor: r.net_minor,
                tax_minor: r.tax_minor,
                gross_minor: r.gross_minor,
            },
        }
    }
}

/// An invoice or credit note, as issued.
#[model]
pub struct Invoice {
    /// e.g. `inv_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    /// `invoice` or `credit_note`.
    pub kind: String,
    /// e.g. `INV-2026-00001`.
    pub number: String,
    pub order_id: String,
    pub order_number: i64,
    /// For a credit note: the invoice it credits.
    pub credits_id: Option<String>,
    pub refund_id: Option<String>,
    /// RFC 3339, UTC.
    pub issued_at: String,
    pub currency: String,
    pub seller: Party,
    pub buyer: Party,
    pub lines: Vec<InvoiceLine>,
    /// Totals per tax rate, highest first.
    pub rates: Vec<RateLine>,
    pub net_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub prices_include_tax: bool,
    pub reverse_charge: bool,
    pub note: String,
    /// For an invoice: what credit notes have taken back so far.
    pub credited_minor: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    kind: String,
    number: String,
    order_id: String,
    order_number: i64,
    credits_id: Option<String>,
    refund_id: Option<String>,
    issued_at: String,
    currency: String,
    seller: SqlJson<Party>,
    buyer: SqlJson<Party>,
    lines: SqlJson<Vec<InvoiceLine>>,
    rates: SqlJson<Vec<RateLine>>,
    net_minor: i64,
    tax_minor: i64,
    total_minor: i64,
    prices_include_tax: bool,
    reverse_charge: bool,
    note: String,
    credited_minor: i64,
}

const COLUMNS: &str = "id, kind, number, order_id, order_number, credits_id, refund_id, \
     to_char(issued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS issued_at, \
     currency, seller, buyer, lines, rates, net_minor, tax_minor, total_minor, \
     prices_include_tax, reverse_charge, note, \
     COALESCE((SELECT SUM(c.total_minor) FROM invoices c WHERE c.credits_id = invoices.id), 0)::BIGINT \
       AS credited_minor";

impl From<Row> for Invoice {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            number: r.number,
            order_id: r.order_id,
            order_number: r.order_number,
            credits_id: r.credits_id,
            refund_id: r.refund_id,
            issued_at: r.issued_at,
            currency: r.currency,
            seller: r.seller.0,
            buyer: r.buyer.0,
            lines: r.lines.0,
            rates: r.rates.0,
            net_minor: r.net_minor,
            tax_minor: r.tax_minor,
            total_minor: r.total_minor,
            prices_include_tax: r.prices_include_tax,
            reverse_charge: r.reverse_charge,
            note: r.note,
            credited_minor: r.credited_minor,
        }
    }
}

async fn settings(conn: &mut PgConnection, scope: &impl Scope) -> Result<Option<InvoiceSettings>> {
    Ok(sqlx::query_as::<_, InvoiceSettings>(
        "SELECT legal_name, address, vat_number, tax_number, email, bank_details, footer, \
         invoice_prefix, credit_note_prefix, issue_on FROM invoice_settings WHERE tenant_id = $1",
    )
    .bind(scope.tenant_id())
    .fetch_optional(&mut *conn)
    .await?)
}

/// One invoice or credit note of the shop.
pub async fn get(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Option<Invoice>> {
    let Ok(id) = InvoiceId::parse(id) else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM invoices WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(Invoice::from))
}

/// An order's invoice and credit notes, oldest first.
pub async fn for_order(
    conn: &mut PgConnection,
    scope: &impl Scope,
    order_id: &str,
) -> Result<Vec<Invoice>> {
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM invoices WHERE tenant_id = $1 AND order_id = $2 ORDER BY id"
    ))
    .bind(scope.tenant_id())
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(Invoice::from).collect())
}

/// The next number of `series` this year.
async fn next_number(
    tx: &mut TenantTx,
    scope: &impl Scope,
    series: &str,
    prefix: &str,
) -> Result<String> {
    let (year, last): (i32, i64) = sqlx::query_as(
        "INSERT INTO invoice_counters (tenant_id, series, year, last) \
         VALUES ($1, $2, EXTRACT(YEAR FROM now() AT TIME ZONE 'UTC')::INTEGER, 1) \
         ON CONFLICT (tenant_id, series, year) DO UPDATE SET last = invoice_counters.last + 1 \
         RETURNING year, last",
    )
    .bind(scope.tenant_id())
    .bind(series)
    .fetch_one(&mut **tx)
    .await?;
    Ok(number(prefix, year, last))
}

fn seller(s: &InvoiceSettings) -> Party {
    Party {
        name: s.legal_name.clone(),
        address: Some(s.address.clone()),
        vat_number: s.vat_number.clone(),
        tax_number: s.tax_number.clone(),
        email: s.email.clone(),
    }
}

fn buyer(o: &Order) -> Party {
    let address = o
        .billing_address
        .clone()
        .or_else(|| o.shipping_address.clone());
    let name = address
        .as_ref()
        .map(|a| {
            let person = format!("{} {}", a.first_name, a.last_name)
                .trim()
                .to_owned();
            if a.company.trim().is_empty() {
                person
            } else {
                a.company.trim().to_owned()
            }
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| o.email.clone());
    Party {
        name,
        address,
        vat_number: o.vat_number.clone(),
        tax_number: None,
        email: Some(o.email.clone()),
    }
}

/// An order's lines and shipping as invoice lines.
fn lines_of(o: &Order) -> Vec<InvoiceLine> {
    let inclusive = o.prices_include_tax;
    let mut lines: Vec<InvoiceLine> = o
        .lines
        .iter()
        .map(|l| {
            let a = Amounts::from_price(l.total_minor - l.discount_minor, l.tax_minor, inclusive);
            InvoiceLine {
                description: l.title.clone(),
                sku: Some(l.sku.clone()),
                quantity: i64::from(l.quantity),
                unit_price_minor: l.unit_price_minor,
                discount_minor: l.discount_minor,
                rate_bps: l.tax_rate_bps,
                net_minor: a.net_minor,
                tax_minor: a.tax_minor,
                gross_minor: a.gross_minor,
            }
        })
        .collect();
    if o.shipping_minor > 0 {
        let a = Amounts::from_price(
            o.shipping_minor - o.shipping_discount_minor,
            o.shipping_tax_minor,
            inclusive,
        );
        lines.push(InvoiceLine {
            description: match &o.shipping_name {
                Some(n) => format!("Shipping: {n}"),
                None => "Shipping".to_owned(),
            },
            sku: None,
            quantity: 1,
            unit_price_minor: o.shipping_minor,
            discount_minor: o.shipping_discount_minor,
            rate_bps: o.shipping_tax_rate_bps,
            net_minor: a.net_minor,
            tax_minor: a.tax_minor,
            gross_minor: a.gross_minor,
        });
    }
    lines
}

#[allow(clippy::too_many_arguments)]
async fn insert(
    tx: &mut TenantTx,
    scope: &impl Scope,
    kind: &str,
    number: &str,
    order: (&str, i64),
    credits: (Option<&str>, Option<&str>),
    currency: &str,
    parties: (&Party, &Party),
    lines: &[InvoiceLine],
    rates: &[RateLine],
    flags: (bool, bool),
    note: &str,
) -> Result<Invoice> {
    let id = InvoiceId::new();
    let sum = total(&rates.iter().map(RateTotal::from).collect::<Vec<_>>());
    sqlx::query(
        "INSERT INTO invoices (id, tenant_id, kind, number, order_id, order_number, credits_id, \
         refund_id, currency, seller, buyer, lines, rates, net_minor, tax_minor, total_minor, \
         prices_include_tax, reverse_charge, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
    )
    .bind(id.as_str())
    .bind(scope.tenant_id())
    .bind(kind)
    .bind(number)
    .bind(order.0)
    .bind(order.1)
    .bind(credits.0)
    .bind(credits.1)
    .bind(currency)
    .bind(SqlJson(parties.0))
    .bind(SqlJson(parties.1))
    .bind(SqlJson(lines))
    .bind(SqlJson(rates))
    .bind(sum.net_minor)
    .bind(sum.tax_minor)
    .bind(sum.gross_minor)
    .bind(flags.0)
    .bind(flags.1)
    .bind(note)
    .execute(&mut **tx)
    .await?;
    let made = get(tx, scope, id.as_str())
        .await?
        .ok_or_else(|| Error::internal("the invoice just issued is missing"))?;
    let event = if kind == "invoice" {
        "invoice.issued"
    } else {
        "credit_note.issued"
    };
    scope.publish(tx, event, &made).await?;
    Ok(made)
}

/// Issues `order`'s invoice in the shop's transaction. A shop without
/// invoice details is a 422 `invoice_settings_missing`; an order already
/// invoiced is a 409 `invoice_exists`. Publishes `invoice.issued`.
pub async fn issue(tx: &mut TenantTx, scope: &impl Scope, order: &Order) -> Result<Invoice> {
    let s = settings(tx, scope).await?.ok_or_else(|| {
        Error::unprocessable("set the shop's invoice details first")
            .with_code("invoice_settings_missing")
    })?;
    let exists: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM invoices WHERE tenant_id = $1 AND order_id = $2 AND kind = 'invoice'",
    )
    .bind(scope.tenant_id())
    .bind(&order.id)
    .fetch_optional(&mut **tx)
    .await?;
    if exists.is_some() {
        return Err(
            Error::conflict("this order already has an invoice").with_code("invoice_exists")
        );
    }
    let lines = lines_of(order);
    let per_rate: Vec<(i64, Amounts)> = lines
        .iter()
        .map(|l| {
            (
                l.rate_bps,
                Amounts {
                    net_minor: l.net_minor,
                    tax_minor: l.tax_minor,
                    gross_minor: l.gross_minor,
                },
            )
        })
        .collect();
    let rates = breakdown(&per_rate);
    if total(&rates).gross_minor != order.total_minor {
        return Err(Error::internal(
            "the invoice lines do not add up to the order total",
        ));
    }
    let rates: Vec<RateLine> = rates.into_iter().map(RateLine::from).collect();
    let number = next_number(tx, scope, "invoice", &s.invoice_prefix).await?;
    let note = if order.reverse_charge {
        REVERSE_CHARGE_NOTE
    } else {
        ""
    };
    insert(
        tx,
        scope,
        "invoice",
        &number,
        (&order.id, order.number),
        (None, None),
        &order.currency,
        (&seller(&s), &buyer(order)),
        &lines,
        &rates,
        (order.prices_include_tax, order.reverse_charge),
        note,
    )
    .await
}

/// Issues a credit note against invoice `invoice_id` for `amount_minor`
/// (default: all not yet credited), shared over its tax rates. More than
/// is left is a 422 `invalid_credit_amount`. Publishes
/// `credit_note.issued`.
pub async fn credit(
    tx: &mut TenantTx,
    scope: &impl Scope,
    invoice_id: &str,
    amount_minor: Option<i64>,
    reason: &str,
    refund_id: Option<&str>,
) -> Result<Invoice> {
    let inv = get(tx, scope, invoice_id)
        .await?
        .filter(|i| i.kind == "invoice")
        .ok_or_else(Error::not_found)?;
    let s = settings(tx, scope).await?.ok_or_else(|| {
        Error::unprocessable("set the shop's invoice details first")
            .with_code("invoice_settings_missing")
    })?;
    let left = inv.total_minor - inv.credited_minor;
    let amount = amount_minor.unwrap_or(left);
    if amount <= 0 || amount > left {
        return Err(Error::unprocessable(format!(
            "a credit note is 1 to {left} (what is not credited yet)"
        ))
        .with_code("invalid_credit_amount"));
    }
    let rates: Vec<RateTotal> = inv.rates.iter().map(RateTotal::from).collect();
    let parts = credit_split(amount, &rates);
    let reason = reason.trim();
    let what = if reason.is_empty() {
        format!("Credit for invoice {}", inv.number)
    } else {
        format!("Credit for invoice {}: {reason}", inv.number)
    };
    let lines: Vec<InvoiceLine> = parts
        .iter()
        .map(|p| InvoiceLine {
            description: what.clone(),
            sku: None,
            quantity: 1,
            unit_price_minor: if inv.prices_include_tax {
                p.amounts.gross_minor
            } else {
                p.amounts.net_minor
            },
            discount_minor: 0,
            rate_bps: p.rate_bps,
            net_minor: p.amounts.net_minor,
            tax_minor: p.amounts.tax_minor,
            gross_minor: p.amounts.gross_minor,
        })
        .collect();
    let rates: Vec<RateLine> = parts.into_iter().map(RateLine::from).collect();
    let number = next_number(tx, scope, "credit_note", &s.credit_note_prefix).await?;
    insert(
        tx,
        scope,
        "credit_note",
        &number,
        (&inv.order_id, inv.order_number),
        (Some(&inv.id), refund_id),
        &inv.currency,
        (&inv.seller, &inv.buyer),
        &lines,
        &rates,
        (inv.prices_include_tax, inv.reverse_charge),
        &inv.note,
    )
    .await
}

/// The shop's invoice details; a 404 until they are set
/// (`invoices:read`).
#[get("/api/invoices/settings", tag = "invoices")]
async fn get_settings(db: Db, tenant: Tenant) -> Result<Json<InvoiceSettings>> {
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let s = settings(&mut tx, &tenant).await?;
    tx.commit().await?;
    s.map(Json).ok_or_else(Error::not_found)
}

/// Set who issues the shop's invoices, the number prefixes and when an
/// order's invoice is issued: `paid`, `placed` or `manual`
/// (`invoices:write`). Publishes `invoice_settings.updated`.
#[put("/api/invoices/settings", tag = "invoices")]
async fn put_settings(
    db: Db,
    tenant: Tenant,
    Valid(Json(s)): Valid<Json<SettingsInput>>,
) -> Result<Json<InvoiceSettings>> {
    let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_settings"));
    let issue_on = s.issue_on.as_deref().unwrap_or("paid");
    if !["paid", "placed", "manual"].contains(&issue_on) {
        return bad("issue_on is paid, placed or manual");
    }
    if !soat_core::address::is_country_code(&s.address.country) {
        return bad("the address country is an ISO 3166-1 alpha-2 code, e.g. DE");
    }
    if let Some(v) = s.vat_number.as_deref().filter(|v| !v.trim().is_empty()) {
        if soat_core::tax::vat_number_country(v).is_none() {
            return bad(&format!("{v} is not an EU VAT number"));
        }
    }
    let prefix_ok = |p: &Option<String>| {
        p.as_deref().is_none_or(|p| {
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_/".contains(c))
        })
    };
    if !prefix_ok(&s.invoice_prefix) || !prefix_ok(&s.credit_note_prefix) {
        return bad("a prefix is letters, digits, - _ and /");
    }
    let mut tx = tenant.begin_for(&db, &INVOICES_WRITE).await?;
    let saved = sqlx::query_as::<_, InvoiceSettings>(
        "INSERT INTO invoice_settings (tenant_id, legal_name, address, vat_number, tax_number, \
         email, bank_details, footer, invoice_prefix, credit_note_prefix, issue_on) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9, 'INV-'), COALESCE($10, 'CN-'), $11) \
         ON CONFLICT (tenant_id) DO UPDATE SET legal_name = EXCLUDED.legal_name, \
         address = EXCLUDED.address, vat_number = EXCLUDED.vat_number, \
         tax_number = EXCLUDED.tax_number, email = EXCLUDED.email, \
         bank_details = EXCLUDED.bank_details, footer = EXCLUDED.footer, \
         invoice_prefix = COALESCE($9, invoice_settings.invoice_prefix), \
         credit_note_prefix = COALESCE($10, invoice_settings.credit_note_prefix), \
         issue_on = EXCLUDED.issue_on, updated_at = now() \
         RETURNING legal_name, address, vat_number, tax_number, email, bank_details, footer, \
         invoice_prefix, credit_note_prefix, issue_on",
    )
    .bind(tenant.id())
    .bind(s.legal_name.trim())
    .bind(SqlJson(&s.address))
    .bind(
        s.vat_number
            .as_deref()
            .map(|v| v.trim().replace(' ', "").to_uppercase()),
    )
    .bind(s.tax_number.as_deref().map(str::trim))
    .bind(s.email.as_deref().map(str::trim))
    .bind(s.bank_details.trim())
    .bind(s.footer.trim())
    .bind(&s.invoice_prefix)
    .bind(&s.credit_note_prefix)
    .bind(issue_on)
    .fetch_one(&mut *tx)
    .await?;
    tenant
        .publish(&mut tx, "invoice_settings.updated", &saved)
        .await?;
    tx.commit().await?;
    Ok(Json(saved))
}

/// Invoices and credit notes, newest first (`invoices:read`). Filter with
/// `?kind=` or `?order_id=`; `?q=` searches numbers.
#[get("/api/invoices", tag = "invoices")]
async fn list_invoices(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Invoice>>> {
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let page: Paged<Row> = TenantListing::new("invoices", COLUMNS)
        .sortable(["id", "number"])
        .default_sort("-id")
        .search(["number"])
        .filter_text("kind")
        .filter_text("order_id")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(Paged {
        items: page.items.into_iter().map(Invoice::from).collect(),
        total: page.total,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// One invoice or credit note (`invoices:read`).
#[get("/api/invoices/{id}", tag = "invoices")]
async fn get_invoice(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Invoice>> {
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let inv = get(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    inv.map(Json).ok_or_else(Error::not_found)
}

/// The invoice as a printable HTML page (`invoices:read`).
#[get("/api/invoices/{id}/document", tag = "invoices")]
async fn invoice_document(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Html<String>> {
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let inv = get(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let page = document::page(&mut tx, &tenant, &inv).await?;
    tx.commit().await?;
    Ok(Html(page))
}

/// Issue an order's invoice now (`invoices:write`): for shops that issue
/// by hand, or before payment. Publishes `invoice.issued`.
#[post("/api/orders/{id}/invoice", tag = "invoices", status = 201)]
async fn issue_for_order(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Invoice>)> {
    let mut tx = tenant.begin_for(&db, &INVOICES_WRITE).await?;
    let order = soat_orders::load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let inv = issue(&mut tx, &tenant, &order).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(inv)))
}

/// Body of issuing a credit note.
#[model]
pub struct NewCreditNote {
    /// Gross; default: all not yet credited.
    #[validate(range(min = 1))]
    pub amount_minor: Option<i64>,
    #[validate(length(max = 200))]
    #[serde(default)]
    pub reason: String,
}

/// Credit an invoice, e.g. to correct it (`invoices:write`). Refunds
/// credit their invoice by themselves. Publishes `credit_note.issued`.
#[post("/api/invoices/{id}/credit-notes", tag = "invoices", status = 201)]
async fn issue_credit_note(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(b)): Valid<Json<NewCreditNote>>,
) -> Result<(StatusCode, Json<Invoice>)> {
    let mut tx = tenant.begin_for(&db, &INVOICES_WRITE).await?;
    let note = credit(&mut tx, &tenant, &id, b.amount_minor, &b.reason, None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(note)))
}

/// An order's invoice and credit notes, for the buyer holding its token
/// (`x-soat-order-token`).
#[get("/api/storefront/orders/{id}/invoices", tag = "storefront")]
async fn buyer_invoices(
    db: Db,
    holder: OrderToken,
    Path(id): Path<String>,
) -> Result<Json<Vec<Invoice>>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let all = for_order(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    Ok(Json(all))
}

/// One of an order's invoices as a printable page, for the buyer holding
/// the order's token.
#[get(
    "/api/storefront/orders/{id}/invoices/{invoice_id}/document",
    tag = "storefront"
)]
async fn buyer_document(
    db: Db,
    holder: OrderToken,
    Path((id, invoice_id)): Path<(String, String)>,
) -> Result<Html<String>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let inv = get(&mut tx, &holder.shop, &invoice_id)
        .await?
        .filter(|i| i.order_id == id)
        .ok_or_else(Error::not_found)?;
    let page = document::page(&mut tx, &holder.shop, &inv).await?;
    tx.commit().await?;
    Ok(Html(page))
}

/// The invoice as an electronic invoice to download (`invoices:read`):
/// `?format=xrechnung` (default; XRechnung 3.0, CII), `cii` (the XML of
/// Factur-X / ZUGFeRD, profile EN 16931), `peppol` (Peppol BIS Billing
/// 3.0, UBL) or `pdf` (Factur-X / ZUGFeRD: a PDF/A-3 that shows the
/// invoice, with the `cii` XML inside). `?buyer_reference=` sets BT-10, such as a Leitweg-ID (default:
/// the order number). See `/e-invoice/check` for what a receiver would
/// reject.
#[get("/api/invoices/{id}/e-invoice", tag = "invoices")]
async fn invoice_xml(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Query(q)): Valid<Query<EInvoiceQuery>>,
) -> Result<Response> {
    q.format()?;
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let inv = get(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let doc = einvoice::document(&mut tx, &tenant, &inv, q.buyer_reference.as_deref()).await?;
    tx.commit().await?;
    einvoice::download(&doc, &q)
}

/// What a receiver's validator would reject in the invoice as an
/// e-invoice in `?format=`, by rule (EN 16931, XRechnung, Peppol), such as
/// a missing seller phone for XRechnung (`invoices:read`).
#[get("/api/invoices/{id}/e-invoice/check", tag = "invoices")]
async fn check_invoice_xml(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Query(q)): Valid<Query<EInvoiceQuery>>,
) -> Result<Json<EInvoiceCheck>> {
    let format = q.format()?;
    let mut tx = tenant.begin_for(&db, &INVOICES_READ).await?;
    let inv = get(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let doc = einvoice::document(&mut tx, &tenant, &inv, q.buyer_reference.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(EInvoiceCheck::new(
        format,
        soat_core::einvoice::check(&doc, format),
    )))
}

/// One of an order's invoices as an electronic invoice, for the buyer
/// holding the order's token; `?format=` and `?buyer_reference=` as for
/// staff.
#[get(
    "/api/storefront/orders/{id}/invoices/{invoice_id}/e-invoice",
    tag = "storefront"
)]
async fn buyer_invoice_xml(
    db: Db,
    holder: OrderToken,
    Path((id, invoice_id)): Path<(String, String)>,
    Valid(Query(q)): Valid<Query<EInvoiceQuery>>,
) -> Result<Response> {
    q.format()?;
    let (mut tx, id) = holder.open(&db, &id).await?;
    let inv = get(&mut tx, &holder.shop, &invoice_id)
        .await?
        .filter(|i| i.order_id == id)
        .ok_or_else(Error::not_found)?;
    let doc = einvoice::document(&mut tx, &holder.shop, &inv, q.buyer_reference.as_deref()).await?;
    tx.commit().await?;
    einvoice::download(&doc, &q)
}

/// The shop an event belongs to; the system acts.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Issues invoices when orders are placed or paid, and credit notes for
/// refunds, in shops that set their invoice details.
struct Invoicing;

impl Subscriber for Invoicing {
    fn name(&self) -> &'static str {
        "invoices"
    }

    fn wants(&self, kind: &str) -> bool {
        matches!(kind, "order.placed" | "order.paid" | "payment.refunded")
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            let scope = EventScope(event.tenant_id.clone());
            let Some(s) = settings(&mut tx, &scope).await? else {
                return Ok(());
            };
            match event.kind.as_str() {
                "order.placed" | "order.paid" => {
                    let wanted = if event.kind == "order.placed" {
                        "placed"
                    } else {
                        "paid"
                    };
                    if s.issue_on != wanted {
                        return Ok(());
                    }
                    let Some(id) = event.payload["id"].as_str() else {
                        return Ok(());
                    };
                    let Some(order) = soat_orders::load(&mut tx, &scope, id).await? else {
                        return Ok(());
                    };
                    match issue(&mut tx, &scope, &order).await {
                        Ok(_) => {}
                        Err(e) if e.status() == StatusCode::CONFLICT => return Ok(()),
                        Err(e) => return Err(e),
                    }
                }
                _ => {
                    let (Some(payment), Some(refund), Some(amount)) = (
                        event.payload["payment_id"].as_str(),
                        event.payload["id"].as_str(),
                        event.payload["amount_minor"].as_i64(),
                    ) else {
                        return Ok(());
                    };
                    let Some(p) = soat_payments::get(&mut tx, &scope, payment).await? else {
                        return Ok(());
                    };
                    let invoices = for_order(&mut tx, &scope, &p.reference).await?;
                    let Some(inv) = invoices.iter().find(|i| i.kind == "invoice") else {
                        return Ok(());
                    };
                    if invoices
                        .iter()
                        .any(|i| i.refund_id.as_deref() == Some(refund))
                    {
                        return Ok(());
                    }
                    let left = inv.total_minor - inv.credited_minor;
                    if left <= 0 {
                        return Ok(());
                    }
                    let reason = event.payload["reason"].as_str().unwrap_or("Refund");
                    let reason = if reason.trim().is_empty() {
                        "Refund"
                    } else {
                        reason
                    };
                    credit(
                        &mut tx,
                        &scope,
                        &inv.id,
                        Some(amount.min(left)),
                        reason,
                        Some(refund),
                    )
                    .await?;
                }
            }
            tx.commit().await?;
            Ok(())
        })
    }
}
