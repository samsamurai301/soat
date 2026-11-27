//! Invoices as electronic invoices (XRechnung, Factur-X / ZUGFeRD XML,
//! Peppol BIS): the issued invoice mapped onto `soat_core::einvoice`.

use soat_core::einvoice::{self, Category, Document, Format, Line, Problem, Tax};
use soat_core::Currency;
use soat_tenancy::Scope;
use zapline::db::sqlx::PgConnection;
use zapline::prelude::*;

use crate::{get, settings, Invoice, Party};

/// How a request asks for an e-invoice.
#[model]
pub struct EInvoiceQuery {
    /// `xrechnung` (default), `cii` (Factur-X / ZUGFeRD XML, also
    /// `zugferd`), `peppol` (UBL), or `pdf` (Factur-X / ZUGFeRD: a PDF/A-3
    /// with the `cii` XML inside).
    pub format: Option<String>,
    /// BT-10, the buyer's reference, such as a German Leitweg-ID; default:
    /// the order number.
    #[validate(length(max = 200))]
    pub buyer_reference: Option<String>,
}

impl EInvoiceQuery {
    /// The format asked for, the PDF's being CII; a 422 `invalid_format`
    /// for an unknown one.
    pub fn format(&self) -> Result<Format> {
        if self.wants_pdf() {
            return Ok(Format::Cii);
        }
        match self.format.as_deref().filter(|f| !f.trim().is_empty()) {
            None => Ok(Format::XRechnung),
            Some(f) => Format::parse(f).ok_or_else(|| {
                Error::unprocessable(
                    "format is xrechnung, cii (ZUGFeRD, Factur-X), peppol or pdf (ZUGFeRD, Factur-X)",
                )
                .with_code("invalid_format")
            }),
        }
    }

    /// Whether the PDF/A-3 with the XML inside is asked for.
    pub fn wants_pdf(&self) -> bool {
        self.format.as_deref().is_some_and(|f| {
            matches!(
                f.trim().to_ascii_lowercase().as_str(),
                "pdf" | "zugferd-pdf" | "facturx-pdf" | "factur-x-pdf"
            )
        })
    }
}

/// One thing a receiver would reject.
#[model]
pub struct EInvoiceProblem {
    /// The EN 16931, XRechnung or Peppol rule, e.g. `BR-DE-15`.
    pub rule: String,
    pub message: String,
}

/// Whether an invoice is ready to send in a format.
#[model]
pub struct EInvoiceCheck {
    pub format: String,
    /// No problems were found.
    pub ready: bool,
    pub problems: Vec<EInvoiceProblem>,
}

impl EInvoiceCheck {
    pub(crate) fn new(format: Format, problems: Vec<Problem>) -> Self {
        Self {
            format: format.name().to_owned(),
            ready: problems.is_empty(),
            problems: problems
                .into_iter()
                .map(|p| EInvoiceProblem {
                    rule: p.rule.to_owned(),
                    message: p.message,
                })
                .collect(),
        }
    }
}

fn party(p: &Party) -> einvoice::Party {
    let a = p.address.clone().unwrap_or_default();
    // The person, when a company is the party, is who to contact.
    let person = format!("{} {}", a.first_name, a.last_name)
        .trim()
        .to_owned();
    einvoice::Party {
        name: p.name.clone(),
        line1: a.line1,
        line2: a.line2,
        postal_code: a.postal_code,
        city: a.city,
        region: a.region,
        country: a.country,
        vat_number: p.vat_number.clone(),
        tax_number: p.tax_number.clone(),
        email: p.email.clone(),
        phone: Some(a.phone).filter(|s| !s.trim().is_empty()),
        contact: if person == p.name.trim() {
            String::new()
        } else {
            person
        },
    }
}

/// `inv` as an e-invoice document, with what the shop's current settings
/// and the order add: the IBAN from the bank details, and what was paid.
pub(crate) async fn document(
    conn: &mut PgConnection,
    scope: &impl Scope,
    inv: &Invoice,
    buyer_reference: Option<&str>,
) -> Result<Document> {
    let currency = Currency::parse(&inv.currency)
        .map_err(|_| Error::internal("the invoice's currency is not ISO 4217"))?;
    let iban = settings(conn, scope)
        .await?
        .and_then(|s| einvoice::iban_in(&s.bank_details));
    let credits_number = match &inv.credits_id {
        Some(id) => get(conn, scope, id).await?.map(|i| i.number),
        None => None,
    };
    let order = soat_orders::load(conn, scope, &inv.order_id).await?;
    let settled = if inv.kind == "invoice" {
        order.as_ref().is_some_and(|o| {
            matches!(
                o.financial_status.as_str(),
                "paid" | "partially_refunded" | "refunded"
            )
        })
    } else {
        inv.refund_id.is_some()
    };
    let seller = party(&inv.seller);
    let buyer = party(&inv.buyer);
    let buyer_country = Some(buyer.country.as_str()).filter(|c| !c.is_empty());
    let category =
        |rate: i64| Category::of(rate, inv.reverse_charge, &seller.country, buyer_country);
    let lines: Vec<Line> = inv
        .lines
        .iter()
        .map(|l| Line {
            name: l.description.clone(),
            sku: l.sku.clone().filter(|s| !s.trim().is_empty()),
            quantity: l.quantity,
            net_minor: l.net_minor,
            rate_bps: l.rate_bps,
            category: category(l.rate_bps),
        })
        .collect();
    // The VAT breakdown from the lines themselves, so a free line still has
    // its rate's total.
    let mut taxes: Vec<Tax> = Vec::new();
    for (line, l) in lines.iter().zip(&inv.lines) {
        match taxes
            .iter_mut()
            .find(|t| t.category == line.category && t.rate_bps == line.rate_bps)
        {
            Some(t) => {
                t.basis_minor += l.net_minor;
                t.tax_minor += l.tax_minor;
            }
            None => taxes.push(Tax {
                category: line.category,
                rate_bps: line.rate_bps,
                basis_minor: l.net_minor,
                tax_minor: l.tax_minor,
            }),
        }
    }
    taxes.sort_by_key(|t| std::cmp::Reverse(t.rate_bps));
    let gross: i64 = taxes.iter().map(|t| t.basis_minor + t.tax_minor).sum();
    let payment_terms = match (inv.kind.as_str(), settled) {
        ("invoice", true) => "Paid.".to_owned(),
        ("invoice", false) => format!("Payable on receipt, quoting {}.", inv.number),
        (_, true) => "Refunded.".to_owned(),
        (_, false) => "Credited to the buyer.".to_owned(),
    };
    let order_number = inv.order_number.to_string();
    Ok(Document {
        credit_note: inv.kind == "credit_note",
        number: inv.number.clone(),
        issue_date: inv.issued_at.chars().take(10).collect(),
        currency,
        buyer_reference: buyer_reference
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(&order_number)
            .to_owned(),
        order_number,
        credits_number,
        note: inv.note.clone(),
        seller,
        buyer,
        lines,
        taxes,
        prepaid_minor: if settled { gross } else { 0 },
        iban,
        payment_terms,
    })
}

/// `doc` as the query asks, as a download named after the invoice: the
/// XML in its format, or the Factur-X / ZUGFeRD PDF.
pub(crate) fn download(doc: &Document, q: &EInvoiceQuery) -> Result<Response> {
    let format = q.format()?;
    let safe: String = doc
        .number
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    let (mut res, content_type, name) = if q.wants_pdf() {
        let pdf = soat_core::facturx::pdf(doc, &einvoice::xml(doc, format));
        (
            pdf.into_response(),
            "application/pdf",
            format!("{safe}.pdf"),
        )
    } else {
        (
            einvoice::xml(doc, format).into_response(),
            "application/xml; charset=utf-8",
            format!("{safe}-{}.xml", format.name()),
        )
    };
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}
