//! ZUGFeRD / Factur-X: an e-invoice as a PDF/A-3 that people read, with
//! its EN 16931 CII XML inside as `factur-x.xml` for machines.
//!
//! ```
//! use soat_core::einvoice::{example, xml, Format};
//! use soat_core::facturx;
//!
//! let doc = example();
//! let pdf = facturx::pdf(&doc, &xml(&doc, Format::Cii));
//! assert!(pdf.starts_with(b"%PDF-1.7"));
//! ```

use crate::einvoice::{Category, Document, Party};
use crate::pdf::{fit, wrap, Attachment, Page, Pdf, Style, A4};

/// The attachment's name, as Factur-X 1.0 and ZUGFeRD 2.1 and later want it.
pub const FILE_NAME: &str = "factur-x.xml";

const LEFT: f32 = 50.0;
const RIGHT: f32 = A4.0 - 50.0;
const TOP: f32 = A4.1 - 60.0;
const BOTTOM: f32 = 70.0;
const SIZE: f32 = 9.5;
const LINE: f32 = 13.0;

/// The Factur-X extension schema and its properties, for the XMP.
fn xmp(conformance: &str) -> String {
    let property = |name: &str, description: &str| {
        format!(
            "<rdf:li rdf:parseType=\"Resource\"><pdfaProperty:name>{name}</pdfaProperty:name>\
             <pdfaProperty:valueType>Text</pdfaProperty:valueType>\
             <pdfaProperty:category>external</pdfaProperty:category>\
             <pdfaProperty:description>{description}</pdfaProperty:description></rdf:li>"
        )
    };
    format!(
        "<rdf:Description rdf:about=\"\" xmlns:pdfaExtension=\"http://www.aiim.org/pdfa/ns/extension/\" \
xmlns:pdfaSchema=\"http://www.aiim.org/pdfa/ns/schema#\" xmlns:pdfaProperty=\"http://www.aiim.org/pdfa/ns/property#\">\
<pdfaExtension:schemas><rdf:Bag><rdf:li rdf:parseType=\"Resource\">\
<pdfaSchema:schema>Factur-X PDFA Extension Schema</pdfaSchema:schema>\
<pdfaSchema:namespaceURI>urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#</pdfaSchema:namespaceURI>\
<pdfaSchema:prefix>fx</pdfaSchema:prefix>\
<pdfaSchema:property><rdf:Seq>{}{}{}{}</rdf:Seq></pdfaSchema:property>\
</rdf:li></rdf:Bag></pdfaExtension:schemas></rdf:Description>\n\
<rdf:Description rdf:about=\"\" xmlns:fx=\"urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#\">\
<fx:DocumentType>INVOICE</fx:DocumentType><fx:DocumentFileName>{FILE_NAME}</fx:DocumentFileName>\
<fx:Version>1.0</fx:Version><fx:ConformanceLevel>{conformance}</fx:ConformanceLevel></rdf:Description>\n",
        property("DocumentFileName", "The name of the embedded XML document"),
        property("DocumentType", "The type of the hybrid document in capital letters, e.g. INVOICE or ORDER"),
        property("Version", "The actual version of the standard applying to the embedded XML document"),
        property("ConformanceLevel", "The conformance level of the embedded XML document"),
    )
}

/// The PDF of `doc` with `cii` (its EN 16931 CII XML, from
/// [`crate::einvoice::xml`] with `Format::Cii`) inside.
pub fn pdf(doc: &Document, cii: &str) -> Vec<u8> {
    let kind = if doc.credit_note {
        "Credit note"
    } else {
        "Invoice"
    };
    let title = format!("{kind} {}", doc.number);
    let mut layout = Layout::new(&title);
    layout.head(doc, kind);
    layout.lines(doc);
    layout.totals(doc);
    layout.terms(doc);
    layout.finish(
        &doc.issue_date,
        &Attachment {
            name: FILE_NAME,
            mime: "text/xml",
            description: "The invoice as Factur-X / ZUGFeRD XML (EN 16931)",
            relationship: "Alternative",
            data: cii.as_bytes(),
        },
    )
}

struct Layout {
    pdf: Pdf,
    y: f32,
    footer: String,
}

fn money(doc: &Document, minor: i64) -> String {
    doc.currency.format(minor)
}

fn rate(bps: i64) -> String {
    let whole = bps / 100;
    match bps % 100 {
        0 => format!("{whole} %"),
        r if r % 10 == 0 => format!("{whole}.{} %", r / 10),
        r => format!("{whole}.{r:02} %"),
    }
}

fn address(p: &Party) -> Vec<String> {
    let mut out = vec![p.name.clone()];
    for l in [&p.line1, &p.line2] {
        if !l.is_empty() {
            out.push(l.clone());
        }
    }
    let city = format!("{} {}", p.postal_code, p.city).trim().to_owned();
    if !city.is_empty() {
        out.push(if p.country.is_empty() {
            city
        } else {
            format!("{city}, {}", p.country)
        });
    }
    if let Some(v) = &p.vat_number {
        out.push(format!("VAT ID {v}"));
    }
    if let Some(t) = &p.tax_number {
        out.push(format!("Tax number {t}"));
    }
    if let Some(e) = &p.email {
        out.push(e.clone());
    }
    out
}

impl Layout {
    fn new(title: &str) -> Self {
        let mut pdf = Pdf::new(title);
        pdf.page();
        Self {
            pdf,
            y: TOP,
            footer: title.to_owned(),
        }
    }

    fn page(&mut self) -> &mut Page {
        self.pdf.current()
    }

    /// Room for `h` points more, or a new page.
    fn need(&mut self, h: f32) {
        if self.y - h < BOTTOM {
            self.pdf.page();
            self.y = TOP;
        }
    }

    fn head(&mut self, doc: &Document, kind: &str) {
        let y = self.y;
        let title = format!("{kind} {}", doc.number);
        self.page().text(Style::Bold, 18.0, LEFT, y, &title);
        let mut meta = format!("Issued {} · Order {}", doc.issue_date, doc.order_number);
        if let Some(n) = &doc.credits_number {
            meta.push_str(&format!(" · Credits invoice {n}"));
        }
        if !doc.buyer_reference.is_empty() {
            meta.push_str(&format!(" · Your reference {}", doc.buyer_reference));
        }
        self.page()
            .text(Style::Regular, SIZE, LEFT, y - 18.0, &meta);
        self.y -= 50.0;
        let seller = address(&doc.seller);
        let buyer = address(&doc.buyer);
        let half = (RIGHT - LEFT) / 2.0;
        let y = self.y;
        for (x, label, lines) in [(LEFT, "From", &seller), (LEFT + half, "To", &buyer)] {
            let page = self.page();
            page.text(Style::Regular, 8.0, x, y, label);
            let mut ly = y - LINE;
            for (i, l) in lines.iter().enumerate() {
                let style = if i == 0 { Style::Bold } else { Style::Regular };
                page.text(style, SIZE, x, ly, &fit(style, SIZE, l, half - 10.0));
                ly -= LINE;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let rows = seller.len().max(buyer.len()) as f32;
        self.y -= LINE * (rows + 1.0) + 14.0;
    }

    fn columns() -> [(f32, &'static str, bool); 4] {
        [
            (LEFT, "Description", false),
            (RIGHT - 170.0, "Qty", true),
            (RIGHT - 100.0, "VAT", true),
            (RIGHT, "Net amount", true),
        ]
    }

    fn table_head(&mut self) {
        let y = self.y;
        let page = self.page();
        for (x, label, right) in Self::columns() {
            if right {
                page.text_right(Style::Bold, SIZE, x, y, label);
            } else {
                page.text(Style::Bold, SIZE, x, y, label);
            }
        }
        page.line(LEFT, y - 4.0, RIGHT, y - 4.0, 0.8, 0.0);
        self.y -= LINE + 4.0;
    }

    fn lines(&mut self, doc: &Document) {
        self.table_head();
        let desc_width = RIGHT - 170.0 - 30.0 - LEFT;
        for line in &doc.lines {
            let mut name = line.name.clone();
            if let Some(sku) = &line.sku {
                name.push_str(&format!(" ({sku})"));
            }
            let wrapped = wrap(Style::Regular, SIZE, &name, desc_width);
            #[allow(clippy::cast_precision_loss)]
            let h = LINE * wrapped.len() as f32 + 4.0;
            if self.y - h < BOTTOM {
                self.pdf.page();
                self.y = TOP;
                self.table_head();
            }
            let y = self.y;
            let cols = Self::columns();
            let page = self.page();
            for (i, w) in wrapped.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                page.text(Style::Regular, SIZE, LEFT, y - LINE * i as f32, w);
            }
            page.text_right(
                Style::Regular,
                SIZE,
                cols[1].0,
                y,
                &line.quantity.to_string(),
            );
            page.text_right(Style::Regular, SIZE, cols[2].0, y, &rate(line.rate_bps));
            page.text_right(
                Style::Regular,
                SIZE,
                cols[3].0,
                y,
                &money(doc, line.net_minor),
            );
            page.line(
                LEFT,
                y - h + LINE - 4.0,
                RIGHT,
                y - h + LINE - 4.0,
                0.3,
                0.75,
            );
            self.y -= h;
        }
        self.y -= 6.0;
    }

    fn totals(&mut self, doc: &Document) {
        let mut rows: Vec<(String, String, Style)> = Vec::new();
        rows.push((
            "Net total".into(),
            money(doc, doc.line_total()),
            Style::Regular,
        ));
        for t in &doc.taxes {
            rows.push((
                format!("VAT {} on {}", rate(t.rate_bps), money(doc, t.basis_minor)),
                money(doc, t.tax_minor),
                Style::Regular,
            ));
        }
        rows.push(("Total".into(), money(doc, doc.grand_total()), Style::Bold));
        if doc.prepaid_minor != 0 {
            rows.push((
                "Paid".into(),
                money(doc, -doc.prepaid_minor),
                Style::Regular,
            ));
            rows.push(("Amount due".into(), money(doc, doc.due()), Style::Bold));
        }
        #[allow(clippy::cast_precision_loss)]
        self.need(LINE * rows.len() as f32 + 10.0);
        let label_x = RIGHT - 260.0;
        for (label, amount, style) in rows {
            let y = self.y;
            let page = self.page();
            if label == "Total" {
                page.line(label_x, y + LINE - 3.0, RIGHT, y + LINE - 3.0, 0.8, 0.0);
            }
            page.text(style, SIZE, label_x, y, &label);
            page.text_right(style, SIZE, RIGHT, y, &amount);
            self.y -= LINE;
        }
        self.y -= 14.0;
    }

    fn paragraph(&mut self, style: Style, text: &str) {
        for l in wrap(style, SIZE, text, RIGHT - LEFT) {
            self.need(LINE);
            let y = self.y;
            self.page().text(style, SIZE, LEFT, y, &l);
            self.y -= LINE;
        }
    }

    fn terms(&mut self, doc: &Document) {
        let mut reasons: Vec<&'static str> = Vec::new();
        for t in &doc.taxes {
            if let Some((_, text)) = t.category.exemption() {
                if !reasons.contains(&text) {
                    reasons.push(text);
                }
            }
        }
        if doc
            .taxes
            .iter()
            .any(|t| t.category == Category::ReverseCharge)
        {
            self.paragraph(Style::Bold, "Reverse charge: the recipient owes the VAT.");
        }
        for r in reasons {
            self.paragraph(Style::Regular, r);
        }
        if !doc.payment_terms.is_empty() {
            self.paragraph(Style::Regular, &doc.payment_terms);
        }
        if let Some(iban) = &doc.iban {
            if doc.due() > 0 {
                let text = format!(
                    "Please transfer {} to IBAN {iban}, quoting {}.",
                    money(doc, doc.due()),
                    doc.number
                );
                self.paragraph(Style::Regular, &text);
            }
        }
        if !doc.note.is_empty() {
            self.y -= 4.0;
            self.paragraph(Style::Regular, &doc.note);
        }
    }

    fn finish(mut self, date: &str, xml: &Attachment<'_>) -> Vec<u8> {
        let pages = self.pdf.pages();
        for n in 0..pages {
            let label = format!(
                "{} · page {} of {pages} · the invoice data is inside this PDF as {FILE_NAME} (Factur-X, ZUGFeRD)",
                self.footer,
                n + 1
            );
            let page = self.pdf.page_at(n);
            page.line(LEFT, 52.0, RIGHT, 52.0, 0.3, 0.75);
            let label = fit(Style::Regular, 7.5, &label, RIGHT - LEFT);
            page.text(Style::Regular, 7.5, LEFT, 40.0, &label);
        }
        self.pdf
            .finish(date, std::slice::from_ref(xml), &xmp("EN 16931"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::einvoice::{example, xml, Format};

    #[test]
    fn the_pdf_carries_the_xml() {
        let doc = example();
        let cii = xml(&doc, Format::Cii);
        let pdf = pdf(&doc, &cii);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("(factur-x.xml)"));
        assert!(text.contains("<fx:ConformanceLevel>EN 16931</fx:ConformanceLevel>"));
        assert!(text.contains("<fx:DocumentFileName>factur-x.xml</fx:DocumentFileName>"));
        assert!(text.contains(&format!("/Size {}", cii.len())));
    }

    #[test]
    fn long_invoices_get_more_pages() {
        let mut doc = example();
        let line = doc.lines[0].clone();
        doc.lines = vec![line; 120];
        let pdf = pdf(&doc, "<x/>");
        let text = String::from_utf8_lossy(&pdf);
        let count = text
            .split("/Type /Pages")
            .nth(1)
            .and_then(|s| s.split("/Count ").nth(1))
            .and_then(|s| s.split(' ').next())
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap();
        assert!(count >= 3, "{count} pages");
    }

    #[test]
    fn rates_read_as_percent() {
        assert_eq!(rate(1900), "19 %");
        assert_eq!(rate(550), "5.5 %");
        assert_eq!(rate(1025), "10.25 %");
        assert_eq!(rate(0), "0 %");
    }
}
