//! Electronic invoices after EN 16931, the European standard that public
//! buyers in the EU must accept and that more and more countries require
//! between businesses: XRechnung (Germany's public sector, as CII),
//! Factur-X / ZUGFeRD (the EN 16931 profile, as CII) and Peppol BIS
//! Billing 3.0 (as UBL, e.g. Belgium's B2B invoices from 2026).
//!
//! [`xml`] writes a [`Document`] in one of the [`Format`]s; [`check`] lists
//! what a receiver's validator would reject, by the rule's name (`BR-DE-15`,
//! `BR-AE-02`, ...), so a shop can fix its details before sending.

use crate::tax::is_eu;
use crate::Currency;

/// The syntaxes and rule sets an invoice can be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// XRechnung 3.0 in UN/CEFACT CII.
    XRechnung,
    /// EN 16931 in CII: the XML of Factur-X and ZUGFeRD (profile EN 16931).
    Cii,
    /// Peppol BIS Billing 3.0 in UBL 2.1.
    Peppol,
}

impl Format {
    /// Every format, as named in [`Format::parse`].
    pub const ALL: [Format; 3] = [Format::XRechnung, Format::Cii, Format::Peppol];

    /// `xrechnung`, `cii` (also `zugferd`, `facturx`) or `peppol` (also
    /// `ubl`).
    ///
    /// ```
    /// use soat_core::einvoice::Format;
    /// assert_eq!(Format::parse("ZUGFeRD"), Some(Format::Cii));
    /// assert_eq!(Format::parse("pdf"), None);
    /// ```
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "xrechnung" => Some(Self::XRechnung),
            "cii" | "zugferd" | "facturx" | "factur-x" | "en16931" => Some(Self::Cii),
            "peppol" | "ubl" => Some(Self::Peppol),
            _ => None,
        }
    }

    /// The name [`Format::parse`] reads back.
    pub fn name(self) -> &'static str {
        match self {
            Self::XRechnung => "xrechnung",
            Self::Cii => "cii",
            Self::Peppol => "peppol",
        }
    }

    /// The specification identifier (BT-24) the document declares.
    pub fn specification(self) -> &'static str {
        match self {
            Self::XRechnung => {
                "urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0"
            }
            Self::Cii => "urn:cen.eu:en16931:2017",
            Self::Peppol => {
                "urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0"
            }
        }
    }
}

/// The business process (BT-23) of XRechnung and Peppol invoices.
pub const PEPPOL_PROCESS: &str = "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0";

/// The VAT category of an amount (UNTDID 5305).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// `S`: VAT at a rate above zero.
    Standard,
    /// `Z`: goods and services taxed at 0%.
    ZeroRated,
    /// `AE`: the buyer owes the VAT (reverse charge).
    ReverseCharge,
    /// `G`: exported outside the EU, free of VAT.
    Export,
}

impl Category {
    /// The category of an amount at `rate_bps`, when the invoice is
    /// `reverse_charge` and goes from `seller_country` to `buyer_country`.
    ///
    /// ```
    /// use soat_core::einvoice::Category;
    /// assert_eq!(Category::of(1900, false, "DE", Some("DE")), Category::Standard);
    /// assert_eq!(Category::of(0, true, "DE", Some("FR")), Category::ReverseCharge);
    /// assert_eq!(Category::of(0, false, "DE", Some("US")), Category::Export);
    /// assert_eq!(Category::of(0, false, "DE", Some("DE")), Category::ZeroRated);
    /// ```
    pub fn of(
        rate_bps: i64,
        reverse_charge: bool,
        seller_country: &str,
        buyer_country: Option<&str>,
    ) -> Self {
        if reverse_charge {
            Self::ReverseCharge
        } else if rate_bps > 0 {
            Self::Standard
        } else if is_eu(seller_country) && buyer_country.is_some_and(|c| !is_eu(c)) {
            Self::Export
        } else {
            Self::ZeroRated
        }
    }

    /// The code, e.g. `S`.
    pub fn code(self) -> &'static str {
        match self {
            Self::Standard => "S",
            Self::ZeroRated => "Z",
            Self::ReverseCharge => "AE",
            Self::Export => "G",
        }
    }

    /// Why no VAT is charged (BT-120, BT-121), where the category needs it.
    pub fn exemption(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::ReverseCharge => Some(("VATEX-EU-AE", "Reverse charge")),
            Self::Export => Some(("VATEX-EU-G", "Export outside the EU")),
            _ => None,
        }
    }
}

/// The seller or the buyer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Party {
    /// The legal name, or the person's.
    pub name: String,
    pub line1: String,
    pub line2: String,
    pub postal_code: String,
    pub city: String,
    pub region: String,
    /// ISO 3166-1 alpha-2; empty when there is no address.
    pub country: String,
    pub vat_number: Option<String>,
    /// A national tax number, e.g. a German Steuernummer.
    pub tax_number: Option<String>,
    /// Also the electronic address (BT-34, BT-49, scheme `EM`).
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Who to contact (BT-41); the party's name when empty.
    pub contact: String,
}

/// One line of the document, after discounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    pub sku: Option<String>,
    pub quantity: i64,
    /// The line's net amount (BT-131).
    pub net_minor: i64,
    pub rate_bps: i64,
    pub category: Category,
}

/// The VAT of one category and rate (BG-23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tax {
    pub category: Category,
    pub rate_bps: i64,
    pub basis_minor: i64,
    pub tax_minor: i64,
}

/// An invoice or credit note, ready to be written. Amounts are positive on
/// both: a credit note says by its type that it takes them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub credit_note: bool,
    /// BT-1, e.g. `INV-2026-00001`.
    pub number: String,
    /// BT-2, `YYYY-MM-DD`.
    pub issue_date: String,
    pub currency: Currency,
    /// BT-10: what the buyer asked to be quoted, such as a German
    /// Leitweg-ID.
    pub buyer_reference: String,
    /// BT-14: the shop's order number.
    pub order_number: String,
    /// BT-25: for a credit note, the invoice it credits.
    pub credits_number: Option<String>,
    /// BT-22.
    pub note: String,
    pub seller: Party,
    pub buyer: Party,
    pub lines: Vec<Line>,
    pub taxes: Vec<Tax>,
    /// BT-113: what was paid already.
    pub prepaid_minor: i64,
    /// BT-84: where to pay by transfer.
    pub iban: Option<String>,
    /// BT-20.
    pub payment_terms: String,
}

impl Document {
    /// BT-106: the sum of the lines' net amounts.
    pub fn line_total(&self) -> i64 {
        self.lines.iter().map(|l| l.net_minor).sum()
    }

    /// BT-110: the sum of the VAT.
    pub fn tax_total(&self) -> i64 {
        self.taxes.iter().map(|t| t.tax_minor).sum()
    }

    /// BT-112: net plus VAT.
    pub fn grand_total(&self) -> i64 {
        self.line_total() + self.tax_total()
    }

    /// BT-115: what is left to pay.
    pub fn due(&self) -> i64 {
        self.grand_total() - self.prepaid_minor
    }
}

/// The first IBAN in `text` (such as a shop's bank details), without its
/// spaces, when its check digits are right.
///
/// ```
/// use soat_core::einvoice::iban_in;
/// let text = "Bank: Example\nIBAN: DE89 3704 0044 0532 0130 00\nBIC: COBADEFFXXX";
/// assert_eq!(iban_in(text).as_deref(), Some("DE89370400440532013000"));
/// assert_eq!(iban_in("IBAN DE00 1234"), None);
/// ```
pub fn iban_in(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for start in 0..words.len() {
        let mut candidate = String::new();
        for w in &words[start..] {
            let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if w.is_empty() || !w.bytes().all(|b| b.is_ascii_alphanumeric()) {
                break;
            }
            candidate.push_str(&w.to_ascii_uppercase());
            if candidate.len() > 34 {
                break;
            }
            if candidate.len() >= 15 && iban_valid(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether `iban` (no spaces, uppercase) has right check digits (ISO 13616).
fn iban_valid(iban: &str) -> bool {
    let b = iban.as_bytes();
    if b.len() < 15
        || b.len() > 34
        || !b[..2].iter().all(u8::is_ascii_uppercase)
        || !b[2..4].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let mut rem: u32 = 0;
    for &c in b[4..].iter().chain(&b[..4]) {
        let v = match c {
            b'0'..=b'9' => u32::from(c - b'0'),
            b'A'..=b'Z' => u32::from(c - b'A') + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

/// Something a receiver's validator would reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The rule, e.g. `BR-DE-15`.
    pub rule: &'static str,
    pub message: String,
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn missing(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(blank)
}

/// What a validator of `format` would reject in `doc`, by rule; empty when
/// it would pass these checks. It checks the rules a shop's own details
/// can break, not the whole schematron.
///
/// ```
/// use soat_core::einvoice::{check, Format};
/// # let doc = soat_core::einvoice::example();
/// let mut doc = doc;
/// doc.buyer_reference.clear();
/// let rules: Vec<_> = check(&doc, Format::XRechnung).into_iter().map(|p| p.rule).collect();
/// assert_eq!(rules, ["BR-DE-15"]);
/// assert!(check(&doc, Format::Cii).is_empty());
/// ```
pub fn check(doc: &Document, format: Format) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut add = |rule: &'static str, message: &str| {
        out.push(Problem {
            rule,
            message: message.to_owned(),
        })
    };
    let (s, b) = (&doc.seller, &doc.buyer);
    if blank(&s.name) {
        add("BR-6", "The seller needs a name.");
    }
    if blank(&b.name) {
        add("BR-7", "The buyer needs a name.");
    }
    if blank(&s.country) {
        add("BR-9", "The seller's address needs a country.");
    }
    if blank(&b.country) {
        add(
            "BR-11",
            "The buyer has no address: an electronic invoice needs at least their country.",
        );
    }
    if doc.lines.is_empty() {
        add("BR-16", "The invoice has no lines.");
    }
    if doc.lines.iter().any(|l| l.quantity <= 0) {
        add("BR-22", "Every line needs a quantity above zero.");
    }
    if doc.credit_note && missing(&doc.credits_number) {
        add("BR-55", "A credit note names the invoice it credits.");
    }
    // Each line's category and rate has its VAT breakdown, with the lines' sum.
    for t in &doc.taxes {
        let basis: i64 = doc
            .lines
            .iter()
            .filter(|l| l.category == t.category && l.rate_bps == t.rate_bps)
            .map(|l| l.net_minor)
            .sum();
        if basis != t.basis_minor {
            add(
                "BR-CO-18",
                "The VAT breakdown does not add up to the lines at its rate.",
            );
        }
        // Validators allow one currency unit of rounding (BR-CO-17).
        let unit = 10_i64.pow(doc.currency.decimals());
        let exact = (i128::from(t.basis_minor) * i128::from(t.rate_bps) + 5_000) / 10_000;
        if (i128::from(t.tax_minor) - exact).abs() > i128::from(unit) {
            add("BR-CO-17", "The VAT is not the rate of its basis.");
        }
        if t.category != Category::Standard && t.tax_minor != 0 {
            add(
                "BR-AE-09",
                "VAT-free categories (reverse charge, export, 0%) charge no VAT.",
            );
        }
    }
    for l in &doc.lines {
        if !doc
            .taxes
            .iter()
            .any(|t| t.category == l.category && t.rate_bps == l.rate_bps)
        {
            add(
                "BR-CO-18",
                "A line's VAT rate has no total in the VAT breakdown.",
            );
        }
    }
    let seller_registered = !missing(&s.vat_number) || !missing(&s.tax_number);
    let has = |c: Category| doc.taxes.iter().any(|t| t.category == c);
    if has(Category::Standard) && !seller_registered {
        add(
            "BR-S-02",
            "Invoices with VAT show the seller's VAT ID or tax number.",
        );
    }
    if has(Category::ReverseCharge) {
        if missing(&s.vat_number) {
            add(
                "BR-AE-02",
                "Reverse-charge invoices show the seller's VAT ID.",
            );
        }
        if missing(&b.vat_number) {
            add(
                "BR-AE-02",
                "Reverse-charge invoices show the buyer's VAT ID.",
            );
        }
    }
    if has(Category::Export) && !seller_registered {
        add(
            "BR-G-02",
            "Export invoices show the seller's VAT ID or tax number.",
        );
    }
    if has(Category::ZeroRated) && !seller_registered {
        add(
            "BR-Z-02",
            "0% invoices show the seller's VAT ID or tax number.",
        );
    }
    if format != Format::Cii {
        if missing(&s.email) {
            add(
                "PEPPOL-EN16931-R020",
                "The seller needs an electronic address: set the shop's invoice email.",
            );
        }
        if missing(&b.email) {
            add(
                "PEPPOL-EN16931-R010",
                "The buyer needs an electronic address (their email).",
            );
        }
    }
    if format == Format::XRechnung {
        if blank(&doc.buyer_reference) {
            add(
                "BR-DE-15",
                "XRechnung needs the buyer's reference, such as their Leitweg-ID.",
            );
        }
        if blank(&s.city) || blank(&s.postal_code) {
            add(
                "BR-DE-3",
                "The seller's address needs a city and postal code.",
            );
        }
        if blank(&b.city) || blank(&b.postal_code) {
            add(
                "BR-DE-8",
                "The buyer's address needs a city and postal code.",
            );
        }
        if missing(&s.phone) {
            add(
                "BR-DE-6",
                "XRechnung needs the seller's phone number: set it in the invoice address.",
            );
        }
        if missing(&s.email) {
            add(
                "BR-DE-7",
                "XRechnung needs the seller's email: set the shop's invoice email.",
            );
        }
        if doc.iban.is_none() && doc.due() > 0 {
            add(
                "BR-DE-23",
                "Put the shop's IBAN in its bank details, so the buyer can pay by transfer.",
            );
        }
    }
    out
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// `1999` in a two-decimal currency as `19.99`.
fn amount(minor: i64, decimals: u32) -> String {
    if decimals == 0 {
        return minor.to_string();
    }
    let unit = 10_i64.pow(decimals);
    let sign = if minor < 0 { "-" } else { "" };
    let m = minor.unsigned_abs();
    let u = unit.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        m / u,
        m % u,
        width = decimals as usize
    )
}

/// `1900` as `19.00`.
fn percent(bps: i64) -> String {
    amount(bps, 2)
}

/// A builder of indented XML.
struct Xml {
    out: String,
    depth: usize,
}

impl Xml {
    fn new() -> Self {
        Self {
            out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
            depth: 0,
        }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
    }

    fn open(&mut self, tag: &str) {
        self.open_with(tag, "");
    }

    fn open_with(&mut self, tag: &str, attrs: &str) {
        self.indent();
        self.out.push('<');
        self.out.push_str(tag);
        self.out.push_str(attrs);
        self.out.push_str(">\n");
        self.depth += 1;
    }

    fn close(&mut self, tag: &str) {
        self.depth -= 1;
        self.indent();
        self.out.push_str("</");
        self.out.push_str(tag);
        self.out.push_str(">\n");
    }

    fn leaf(&mut self, tag: &str, text: &str) {
        self.leaf_with(tag, "", text);
    }

    fn leaf_with(&mut self, tag: &str, attrs: &str, text: &str) {
        self.indent();
        self.out
            .push_str(&format!("<{tag}{attrs}>{}</{tag}>\n", esc(text)));
    }

    /// A leaf, unless `text` is blank.
    fn some(&mut self, tag: &str, text: &str) {
        if !blank(text) {
            self.leaf(tag, text.trim());
        }
    }

    fn empty(&mut self, tag: &str) {
        self.indent();
        self.out.push_str(&format!("<{tag}/>\n"));
    }
}

fn opt(s: &Option<String>) -> &str {
    s.as_deref().map(str::trim).unwrap_or("")
}

/// `doc` as XML in `format`.
///
/// ```
/// use soat_core::einvoice::{xml, Format};
/// let doc = soat_core::einvoice::example();
/// let out = xml(&doc, Format::XRechnung);
/// assert!(out.contains("<ram:ID>urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0</ram:ID>"));
/// assert!(xml(&doc, Format::Peppol).contains("<cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>"));
/// ```
pub fn xml(doc: &Document, format: Format) -> String {
    match format {
        Format::Peppol => ubl(doc),
        _ => cii(doc, format),
    }
}

const CII_NS: &str = " xmlns:rsm=\"urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100\" \
xmlns:ram=\"urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100\" \
xmlns:qdt=\"urn:un:unece:uncefact:data:standard:QualifiedDataType:100\" \
xmlns:udt=\"urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100\"";

fn cii_party(x: &mut Xml, tag: &str, p: &Party, seller: bool) {
    x.open(tag);
    x.leaf("ram:Name", p.name.trim());
    if seller {
        x.open("ram:DefinedTradeContact");
        x.leaf(
            "ram:PersonName",
            if blank(&p.contact) {
                p.name.trim()
            } else {
                p.contact.trim()
            },
        );
        if !missing(&p.phone) {
            x.open("ram:TelephoneUniversalCommunication");
            x.leaf("ram:CompleteNumber", opt(&p.phone));
            x.close("ram:TelephoneUniversalCommunication");
        }
        if !missing(&p.email) {
            x.open("ram:EmailURIUniversalCommunication");
            x.leaf("ram:URIID", opt(&p.email));
            x.close("ram:EmailURIUniversalCommunication");
        }
        x.close("ram:DefinedTradeContact");
    }
    if !blank(&p.country) {
        x.open("ram:PostalTradeAddress");
        x.some("ram:PostcodeCode", &p.postal_code);
        x.some("ram:LineOne", &p.line1);
        x.some("ram:LineTwo", &p.line2);
        x.some("ram:CityName", &p.city);
        x.leaf("ram:CountryID", p.country.trim());
        x.some("ram:CountrySubDivisionName", &p.region);
        x.close("ram:PostalTradeAddress");
    }
    if !missing(&p.email) {
        x.open("ram:URIUniversalCommunication");
        x.leaf_with("ram:URIID", " schemeID=\"EM\"", opt(&p.email));
        x.close("ram:URIUniversalCommunication");
    }
    for (scheme, id) in [("VA", &p.vat_number), ("FC", &p.tax_number)] {
        if !missing(id) {
            x.open("ram:SpecifiedTaxRegistration");
            x.leaf_with("ram:ID", &format!(" schemeID=\"{scheme}\""), opt(id));
            x.close("ram:SpecifiedTaxRegistration");
        }
    }
    x.close(tag);
}

/// The net price of one unit, and the quantity it is the price of: a whole
/// price per unit when the net divides, else the whole line's net for its
/// quantity, so price × quantity is the net exactly.
fn price(l: &Line) -> (i64, i64) {
    if l.quantity > 0 && l.net_minor % l.quantity == 0 {
        (l.net_minor / l.quantity, 1)
    } else {
        (l.net_minor, l.quantity.max(1))
    }
}

fn cii(doc: &Document, format: Format) -> String {
    let d = doc.currency.decimals();
    let cur = doc.currency.as_str();
    let mut x = Xml::new();
    x.open_with("rsm:CrossIndustryInvoice", CII_NS);
    x.open("rsm:ExchangedDocumentContext");
    if format == Format::XRechnung {
        x.open("ram:BusinessProcessSpecifiedDocumentContextParameter");
        x.leaf("ram:ID", PEPPOL_PROCESS);
        x.close("ram:BusinessProcessSpecifiedDocumentContextParameter");
    }
    x.open("ram:GuidelineSpecifiedDocumentContextParameter");
    x.leaf("ram:ID", format.specification());
    x.close("ram:GuidelineSpecifiedDocumentContextParameter");
    x.close("rsm:ExchangedDocumentContext");

    x.open("rsm:ExchangedDocument");
    x.leaf("ram:ID", &doc.number);
    x.leaf("ram:TypeCode", if doc.credit_note { "381" } else { "380" });
    x.open("ram:IssueDateTime");
    x.leaf_with(
        "udt:DateTimeString",
        " format=\"102\"",
        &doc.issue_date.replace('-', ""),
    );
    x.close("ram:IssueDateTime");
    if !blank(&doc.note) {
        x.open("ram:IncludedNote");
        x.leaf("ram:Content", doc.note.trim());
        x.close("ram:IncludedNote");
    }
    x.close("rsm:ExchangedDocument");

    x.open("rsm:SupplyChainTradeTransaction");
    for (i, l) in doc.lines.iter().enumerate() {
        x.open("ram:IncludedSupplyChainTradeLineItem");
        x.open("ram:AssociatedDocumentLineDocument");
        x.leaf("ram:LineID", &(i + 1).to_string());
        x.close("ram:AssociatedDocumentLineDocument");
        x.open("ram:SpecifiedTradeProduct");
        x.some("ram:SellerAssignedID", opt(&l.sku));
        x.leaf("ram:Name", l.name.trim());
        x.close("ram:SpecifiedTradeProduct");
        let (unit, basis) = price(l);
        x.open("ram:SpecifiedLineTradeAgreement");
        x.open("ram:NetPriceProductTradePrice");
        x.leaf("ram:ChargeAmount", &amount(unit, d));
        x.leaf_with("ram:BasisQuantity", " unitCode=\"C62\"", &basis.to_string());
        x.close("ram:NetPriceProductTradePrice");
        x.close("ram:SpecifiedLineTradeAgreement");
        x.open("ram:SpecifiedLineTradeDelivery");
        x.leaf_with(
            "ram:BilledQuantity",
            " unitCode=\"C62\"",
            &l.quantity.to_string(),
        );
        x.close("ram:SpecifiedLineTradeDelivery");
        x.open("ram:SpecifiedLineTradeSettlement");
        x.open("ram:ApplicableTradeTax");
        x.leaf("ram:TypeCode", "VAT");
        x.leaf("ram:CategoryCode", l.category.code());
        x.leaf("ram:RateApplicablePercent", &percent(l.rate_bps));
        x.close("ram:ApplicableTradeTax");
        x.open("ram:SpecifiedTradeSettlementLineMonetarySummation");
        x.leaf("ram:LineTotalAmount", &amount(l.net_minor, d));
        x.close("ram:SpecifiedTradeSettlementLineMonetarySummation");
        x.close("ram:SpecifiedLineTradeSettlement");
        x.close("ram:IncludedSupplyChainTradeLineItem");
    }

    x.open("ram:ApplicableHeaderTradeAgreement");
    x.some("ram:BuyerReference", &doc.buyer_reference);
    cii_party(&mut x, "ram:SellerTradeParty", &doc.seller, true);
    cii_party(&mut x, "ram:BuyerTradeParty", &doc.buyer, false);
    if !blank(&doc.order_number) {
        x.open("ram:SellerOrderReferencedDocument");
        x.leaf("ram:IssuerAssignedID", doc.order_number.trim());
        x.close("ram:SellerOrderReferencedDocument");
    }
    x.close("ram:ApplicableHeaderTradeAgreement");
    x.empty("ram:ApplicableHeaderTradeDelivery");

    x.open("ram:ApplicableHeaderTradeSettlement");
    x.leaf("ram:PaymentReference", &doc.number);
    x.leaf("ram:InvoiceCurrencyCode", cur);
    x.open("ram:SpecifiedTradeSettlementPaymentMeans");
    match &doc.iban {
        Some(iban) => {
            x.leaf("ram:TypeCode", "58");
            x.open("ram:PayeePartyCreditorFinancialAccount");
            x.leaf("ram:IBANID", iban);
            x.close("ram:PayeePartyCreditorFinancialAccount");
        }
        None => x.leaf("ram:TypeCode", "1"),
    }
    x.close("ram:SpecifiedTradeSettlementPaymentMeans");
    for t in &doc.taxes {
        x.open("ram:ApplicableTradeTax");
        x.leaf("ram:CalculatedAmount", &amount(t.tax_minor, d));
        x.leaf("ram:TypeCode", "VAT");
        if let Some((_, reason)) = t.category.exemption() {
            x.leaf("ram:ExemptionReason", reason);
        }
        x.leaf("ram:BasisAmount", &amount(t.basis_minor, d));
        x.leaf("ram:CategoryCode", t.category.code());
        if let Some((code, _)) = t.category.exemption() {
            x.leaf("ram:ExemptionReasonCode", code);
        }
        x.leaf("ram:RateApplicablePercent", &percent(t.rate_bps));
        x.close("ram:ApplicableTradeTax");
    }
    if !blank(&doc.payment_terms) {
        x.open("ram:SpecifiedTradePaymentTerms");
        x.leaf("ram:Description", doc.payment_terms.trim());
        x.close("ram:SpecifiedTradePaymentTerms");
    }
    x.open("ram:SpecifiedTradeSettlementHeaderMonetarySummation");
    x.leaf("ram:LineTotalAmount", &amount(doc.line_total(), d));
    x.leaf("ram:TaxBasisTotalAmount", &amount(doc.line_total(), d));
    x.leaf_with(
        "ram:TaxTotalAmount",
        &format!(" currencyID=\"{cur}\""),
        &amount(doc.tax_total(), d),
    );
    x.leaf("ram:GrandTotalAmount", &amount(doc.grand_total(), d));
    x.leaf("ram:TotalPrepaidAmount", &amount(doc.prepaid_minor, d));
    x.leaf("ram:DuePayableAmount", &amount(doc.due(), d));
    x.close("ram:SpecifiedTradeSettlementHeaderMonetarySummation");
    if let Some(n) = doc.credits_number.as_deref().filter(|n| !blank(n)) {
        x.open("ram:InvoiceReferencedDocument");
        x.leaf("ram:IssuerAssignedID", n.trim());
        x.close("ram:InvoiceReferencedDocument");
    }
    x.close("ram:ApplicableHeaderTradeSettlement");
    x.close("rsm:SupplyChainTradeTransaction");
    x.close("rsm:CrossIndustryInvoice");
    x.out
}

const UBL_NS: &str =
    " xmlns:cac=\"urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2\" \
xmlns:cbc=\"urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2\"";

fn ubl_party(x: &mut Xml, tag: &str, p: &Party) {
    x.open(tag);
    x.open("cac:Party");
    if !missing(&p.email) {
        x.leaf_with("cbc:EndpointID", " schemeID=\"EM\"", opt(&p.email));
    }
    if !blank(&p.country) {
        x.open("cac:PostalAddress");
        x.some("cbc:StreetName", &p.line1);
        x.some("cbc:AdditionalStreetName", &p.line2);
        x.some("cbc:CityName", &p.city);
        x.some("cbc:PostalZone", &p.postal_code);
        x.some("cbc:CountrySubentity", &p.region);
        x.open("cac:Country");
        x.leaf("cbc:IdentificationCode", p.country.trim());
        x.close("cac:Country");
        x.close("cac:PostalAddress");
    }
    for (scheme, id) in [("VAT", &p.vat_number), ("TAX", &p.tax_number)] {
        if !missing(id) {
            x.open("cac:PartyTaxScheme");
            x.leaf("cbc:CompanyID", opt(id));
            x.open("cac:TaxScheme");
            x.leaf("cbc:ID", scheme);
            x.close("cac:TaxScheme");
            x.close("cac:PartyTaxScheme");
        }
    }
    x.open("cac:PartyLegalEntity");
    x.leaf("cbc:RegistrationName", p.name.trim());
    x.close("cac:PartyLegalEntity");
    if !missing(&p.phone) || !missing(&p.email) || !blank(&p.contact) {
        x.open("cac:Contact");
        x.some("cbc:Name", &p.contact);
        x.some("cbc:Telephone", opt(&p.phone));
        x.some("cbc:ElectronicMail", opt(&p.email));
        x.close("cac:Contact");
    }
    x.close("cac:Party");
    x.close(tag);
}

fn ubl_category(x: &mut Xml, tag: &str, category: Category, rate_bps: i64, reason: bool) {
    x.open(tag);
    x.leaf("cbc:ID", category.code());
    x.leaf("cbc:Percent", &percent(rate_bps));
    if reason {
        if let Some((code, text)) = category.exemption() {
            x.leaf("cbc:TaxExemptionReasonCode", code);
            x.leaf("cbc:TaxExemptionReason", text);
        }
    }
    x.open("cac:TaxScheme");
    x.leaf("cbc:ID", "VAT");
    x.close("cac:TaxScheme");
    x.close(tag);
}

fn ubl(doc: &Document) -> String {
    let d = doc.currency.decimals();
    let cur = doc.currency.as_str();
    let money = format!(" currencyID=\"{cur}\"");
    let (root, type_tag, line_tag, qty_tag) = if doc.credit_note {
        (
            "CreditNote",
            "cbc:CreditNoteTypeCode",
            "cac:CreditNoteLine",
            "cbc:CreditedQuantity",
        )
    } else {
        (
            "Invoice",
            "cbc:InvoiceTypeCode",
            "cac:InvoiceLine",
            "cbc:InvoicedQuantity",
        )
    };
    let mut x = Xml::new();
    x.open_with(
        root,
        &format!(" xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:{root}-2\"{UBL_NS}"),
    );
    x.leaf("cbc:CustomizationID", Format::Peppol.specification());
    x.leaf("cbc:ProfileID", PEPPOL_PROCESS);
    x.leaf("cbc:ID", &doc.number);
    x.leaf("cbc:IssueDate", &doc.issue_date);
    x.leaf(type_tag, if doc.credit_note { "381" } else { "380" });
    x.some("cbc:Note", &doc.note);
    x.leaf("cbc:DocumentCurrencyCode", cur);
    x.some("cbc:BuyerReference", &doc.buyer_reference);
    if let Some(n) = doc.credits_number.as_deref().filter(|n| !blank(n)) {
        x.open("cac:BillingReference");
        x.open("cac:InvoiceDocumentReference");
        x.leaf("cbc:ID", n.trim());
        x.close("cac:InvoiceDocumentReference");
        x.close("cac:BillingReference");
    }
    ubl_party(&mut x, "cac:AccountingSupplierParty", &doc.seller);
    ubl_party(&mut x, "cac:AccountingCustomerParty", &doc.buyer);
    x.open("cac:PaymentMeans");
    match &doc.iban {
        Some(iban) => {
            x.leaf("cbc:PaymentMeansCode", "58");
            x.leaf("cbc:PaymentID", &doc.number);
            x.open("cac:PayeeFinancialAccount");
            x.leaf("cbc:ID", iban);
            x.close("cac:PayeeFinancialAccount");
        }
        None => {
            x.leaf("cbc:PaymentMeansCode", "1");
            x.leaf("cbc:PaymentID", &doc.number);
        }
    }
    x.close("cac:PaymentMeans");
    if !blank(&doc.payment_terms) {
        x.open("cac:PaymentTerms");
        x.leaf("cbc:Note", doc.payment_terms.trim());
        x.close("cac:PaymentTerms");
    }
    x.open("cac:TaxTotal");
    x.leaf_with("cbc:TaxAmount", &money, &amount(doc.tax_total(), d));
    for t in &doc.taxes {
        x.open("cac:TaxSubtotal");
        x.leaf_with("cbc:TaxableAmount", &money, &amount(t.basis_minor, d));
        x.leaf_with("cbc:TaxAmount", &money, &amount(t.tax_minor, d));
        ubl_category(&mut x, "cac:TaxCategory", t.category, t.rate_bps, true);
        x.close("cac:TaxSubtotal");
    }
    x.close("cac:TaxTotal");
    x.open("cac:LegalMonetaryTotal");
    x.leaf_with(
        "cbc:LineExtensionAmount",
        &money,
        &amount(doc.line_total(), d),
    );
    x.leaf_with(
        "cbc:TaxExclusiveAmount",
        &money,
        &amount(doc.line_total(), d),
    );
    x.leaf_with(
        "cbc:TaxInclusiveAmount",
        &money,
        &amount(doc.grand_total(), d),
    );
    x.leaf_with("cbc:PrepaidAmount", &money, &amount(doc.prepaid_minor, d));
    x.leaf_with("cbc:PayableAmount", &money, &amount(doc.due(), d));
    x.close("cac:LegalMonetaryTotal");
    for (i, l) in doc.lines.iter().enumerate() {
        x.open(line_tag);
        x.leaf("cbc:ID", &(i + 1).to_string());
        x.leaf_with(qty_tag, " unitCode=\"C62\"", &l.quantity.to_string());
        x.leaf_with("cbc:LineExtensionAmount", &money, &amount(l.net_minor, d));
        x.open("cac:Item");
        x.leaf("cbc:Name", l.name.trim());
        if !missing(&l.sku) {
            x.open("cac:SellersItemIdentification");
            x.leaf("cbc:ID", opt(&l.sku));
            x.close("cac:SellersItemIdentification");
        }
        ubl_category(
            &mut x,
            "cac:ClassifiedTaxCategory",
            l.category,
            l.rate_bps,
            false,
        );
        x.close("cac:Item");
        let (unit, basis) = price(l);
        x.open("cac:Price");
        x.leaf_with("cbc:PriceAmount", &money, &amount(unit, d));
        if basis != 1 {
            x.leaf_with("cbc:BaseQuantity", " unitCode=\"C62\"", &basis.to_string());
        }
        x.close("cac:Price");
        x.close(line_tag);
    }
    x.close(root);
    x.out
}

/// A complete, valid invoice, for examples and tests.
#[doc(hidden)]
pub fn example() -> Document {
    Document {
        credit_note: false,
        number: "INV-2026-00001".into(),
        issue_date: "2026-11-27".into(),
        currency: Currency::parse("EUR").expect("EUR"),
        buyer_reference: "04011000-12345-34".into(),
        order_number: "1001".into(),
        credits_number: None,
        note: String::new(),
        seller: Party {
            name: "Beispiel GmbH".into(),
            line1: "Hauptstraße 1".into(),
            postal_code: "10115".into(),
            city: "Berlin".into(),
            country: "DE".into(),
            vat_number: Some("DE123456789".into()),
            email: Some("rechnung@beispiel.de".into()),
            phone: Some("+49 30 123456".into()),
            ..Party::default()
        },
        buyer: Party {
            name: "Stadt Musterhausen".into(),
            line1: "Rathausplatz 2".into(),
            postal_code: "20095".into(),
            city: "Hamburg".into(),
            country: "DE".into(),
            email: Some("einkauf@musterhausen.de".into()),
            ..Party::default()
        },
        lines: vec![
            Line {
                name: "Notebook & pen".into(),
                sku: Some("NB-1".into()),
                quantity: 3,
                net_minor: 2_000,
                rate_bps: 1_900,
                category: Category::Standard,
            },
            Line {
                name: "Book".into(),
                sku: None,
                quantity: 2,
                net_minor: 1_000,
                rate_bps: 700,
                category: Category::Standard,
            },
        ],
        taxes: vec![
            Tax {
                category: Category::Standard,
                rate_bps: 1_900,
                basis_minor: 2_000,
                tax_minor: 380,
            },
            Tax {
                category: Category::Standard,
                rate_bps: 700,
                basis_minor: 1_000,
                tax_minor: 70,
            },
        ],
        prepaid_minor: 0,
        iban: Some("DE89370400440532013000".into()),
        payment_terms: "Payable within 14 days.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(doc: &Document, f: Format) -> Vec<&'static str> {
        check(doc, f).into_iter().map(|p| p.rule).collect()
    }

    #[test]
    fn amounts_and_rates() {
        assert_eq!(amount(1999, 2), "19.99");
        assert_eq!(amount(5, 2), "0.05");
        assert_eq!(amount(-150, 2), "-1.50");
        assert_eq!(amount(500, 0), "500");
        assert_eq!(amount(1234, 3), "1.234");
        assert_eq!(percent(1900), "19.00");
        assert_eq!(percent(550), "5.50");
    }

    #[test]
    fn prices_multiply_back_to_the_net() {
        let mut l = example().lines[0].clone();
        assert_eq!(price(&l), (2000, 3));
        l.net_minor = 2001;
        assert_eq!(price(&l), (667, 1));
    }

    #[test]
    fn ibans() {
        assert_eq!(
            iban_in("IBAN: GB82 WEST 1234 5698 7654 32, BIC WESTGB2L").as_deref(),
            Some("GB82WEST12345698765432")
        );
        assert_eq!(
            iban_in("de89370400440532013000").as_deref(),
            Some("DE89370400440532013000")
        );
        assert_eq!(iban_in("DE89 3704 0044 0532 0130 01"), None);
        assert_eq!(iban_in(""), None);
    }

    #[test]
    fn a_complete_invoice_passes() {
        let doc = example();
        for f in Format::ALL {
            assert_eq!(rules(&doc, f), Vec::<&str>::new(), "{f:?}");
        }
        assert_eq!(doc.line_total(), 3000);
        assert_eq!(doc.grand_total(), 3450);
        assert_eq!(doc.due(), 3450);
    }

    #[test]
    fn missing_details_are_named_by_rule() {
        let mut doc = example();
        doc.seller.phone = None;
        doc.seller.email = None;
        doc.buyer = Party {
            name: "Jo".into(),
            ..Party::default()
        };
        doc.iban = None;
        assert_eq!(
            rules(&doc, Format::XRechnung),
            [
                "BR-11",
                "PEPPOL-EN16931-R020",
                "PEPPOL-EN16931-R010",
                "BR-DE-8",
                "BR-DE-6",
                "BR-DE-7",
                "BR-DE-23"
            ]
        );
        assert_eq!(rules(&doc, Format::Cii), ["BR-11"]);
        doc.prepaid_minor = doc.grand_total();
        assert!(!rules(&doc, Format::XRechnung).contains(&"BR-DE-23"));
    }

    #[test]
    fn vat_categories_need_their_ids() {
        let mut doc = example();
        doc.seller.vat_number = None;
        assert_eq!(rules(&doc, Format::Cii), ["BR-S-02"]);
        doc.seller.tax_number = Some("30/123/45678".into());
        assert!(rules(&doc, Format::Cii).is_empty());

        let mut rc = example();
        for l in &mut rc.lines {
            l.category = Category::ReverseCharge;
            l.rate_bps = 0;
        }
        rc.taxes = vec![Tax {
            category: Category::ReverseCharge,
            rate_bps: 0,
            basis_minor: 3000,
            tax_minor: 0,
        }];
        assert_eq!(rules(&rc, Format::Cii), ["BR-AE-02"]);
        rc.buyer.vat_number = Some("FR12345678901".into());
        assert!(rules(&rc, Format::Cii).is_empty());
        let out = xml(&rc, Format::Cii);
        assert!(out.contains("<ram:ExemptionReasonCode>VATEX-EU-AE</ram:ExemptionReasonCode>"));
        assert!(out.contains("<ram:CategoryCode>AE</ram:CategoryCode>"));
    }

    #[test]
    fn totals_must_add_up() {
        let mut doc = example();
        doc.taxes[0].tax_minor = 500;
        assert_eq!(rules(&doc, Format::Cii), ["BR-CO-17"]);
        // A cent of rounding passes.
        doc.taxes[0].tax_minor = 381;
        assert!(rules(&doc, Format::Cii).is_empty());
        doc.taxes[0].basis_minor = 1999;
        assert_eq!(rules(&doc, Format::Cii), ["BR-CO-18"]);
        doc.taxes.remove(0);
        assert_eq!(rules(&doc, Format::Cii), ["BR-CO-18"]);
    }

    #[test]
    fn cii_has_the_elements_in_schema_order() {
        let out = xml(&example(), Format::XRechnung);
        let order = [
            "<ram:BusinessProcessSpecifiedDocumentContextParameter>",
            "<ram:GuidelineSpecifiedDocumentContextParameter>",
            "<ram:TypeCode>380</ram:TypeCode>",
            "<udt:DateTimeString format=\"102\">20261127</udt:DateTimeString>",
            "<ram:Name>Notebook &amp; pen</ram:Name>",
            "<ram:ChargeAmount>20.00</ram:ChargeAmount>",
            "<ram:BasisQuantity unitCode=\"C62\">3</ram:BasisQuantity>",
            "<ram:BilledQuantity unitCode=\"C62\">3</ram:BilledQuantity>",
            "<ram:RateApplicablePercent>19.00</ram:RateApplicablePercent>",
            "<ram:LineTotalAmount>20.00</ram:LineTotalAmount>",
            "<ram:BuyerReference>04011000-12345-34</ram:BuyerReference>",
            "<ram:SellerTradeParty>",
            "<ram:CompleteNumber>+49 30 123456</ram:CompleteNumber>",
            "<ram:PostcodeCode>10115</ram:PostcodeCode>",
            "<ram:URIID schemeID=\"EM\">rechnung@beispiel.de</ram:URIID>",
            "<ram:ID schemeID=\"VA\">DE123456789</ram:ID>",
            "<ram:BuyerTradeParty>",
            "<ram:IssuerAssignedID>1001</ram:IssuerAssignedID>",
            "<ram:ApplicableHeaderTradeDelivery/>",
            "<ram:InvoiceCurrencyCode>EUR</ram:InvoiceCurrencyCode>",
            "<ram:IBANID>DE89370400440532013000</ram:IBANID>",
            "<ram:CalculatedAmount>3.80</ram:CalculatedAmount>",
            "<ram:BasisAmount>20.00</ram:BasisAmount>",
            "<ram:Description>Payable within 14 days.</ram:Description>",
            "<ram:TaxTotalAmount currencyID=\"EUR\">4.50</ram:TaxTotalAmount>",
            "<ram:GrandTotalAmount>34.50</ram:GrandTotalAmount>",
            "<ram:DuePayableAmount>34.50</ram:DuePayableAmount>",
        ];
        let mut at = 0;
        for part in order {
            let found = out[at..]
                .find(part)
                .unwrap_or_else(|| panic!("{part} missing or out of order in\n{out}"));
            at += found + part.len();
        }
        // 10.00 for 2 divides: the price is per unit.
        assert!(out.contains("<ram:ChargeAmount>5.00</ram:ChargeAmount>"));
        assert!(!xml(&example(), Format::Cii).contains("BusinessProcess"));
    }

    #[test]
    fn credit_notes() {
        let mut doc = example();
        doc.credit_note = true;
        assert_eq!(rules(&doc, Format::Cii), ["BR-55"]);
        doc.credits_number = Some("INV-2026-00001".into());
        doc.number = "CN-2026-00001".into();
        let cii = xml(&doc, Format::Cii);
        assert!(cii.contains("<ram:TypeCode>381</ram:TypeCode>"));
        let reference = cii
            .find("<ram:InvoiceReferencedDocument>")
            .expect("reference");
        assert!(cii[reference..].contains("<ram:IssuerAssignedID>INV-2026-00001"));
        let ubl = xml(&doc, Format::Peppol);
        assert!(ubl.contains(
            "<CreditNote xmlns=\"urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2\""
        ));
        assert!(ubl.contains("<cbc:CreditNoteTypeCode>381</cbc:CreditNoteTypeCode>"));
        assert!(ubl.contains("<cac:CreditNoteLine>"));
        assert!(ubl.contains("<cbc:CreditedQuantity unitCode=\"C62\">3</cbc:CreditedQuantity>"));
        assert!(ubl.contains("<cbc:ID>INV-2026-00001</cbc:ID>"));
    }

    #[test]
    fn ubl_invoice() {
        let out = xml(&example(), Format::Peppol);
        let order = [
            "<cbc:CustomizationID>urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0</cbc:CustomizationID>",
            "<cbc:ProfileID>urn:fdc:peppol.eu:2017:poacc:billing:01:1.0</cbc:ProfileID>",
            "<cbc:IssueDate>2026-11-27</cbc:IssueDate>",
            "<cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>",
            "<cbc:DocumentCurrencyCode>EUR</cbc:DocumentCurrencyCode>",
            "<cbc:BuyerReference>",
            "<cbc:EndpointID schemeID=\"EM\">rechnung@beispiel.de</cbc:EndpointID>",
            "<cbc:CompanyID>DE123456789</cbc:CompanyID>",
            "<cbc:RegistrationName>Beispiel GmbH</cbc:RegistrationName>",
            "<cac:AccountingCustomerParty>",
            "<cbc:PaymentMeansCode>58</cbc:PaymentMeansCode>",
            "<cbc:TaxAmount currencyID=\"EUR\">4.50</cbc:TaxAmount>",
            "<cbc:TaxableAmount currencyID=\"EUR\">20.00</cbc:TaxableAmount>",
            "<cbc:PayableAmount currencyID=\"EUR\">34.50</cbc:PayableAmount>",
            "<cbc:InvoicedQuantity unitCode=\"C62\">3</cbc:InvoicedQuantity>",
            "<cbc:PriceAmount currencyID=\"EUR\">20.00</cbc:PriceAmount>",
            "<cbc:BaseQuantity unitCode=\"C62\">3</cbc:BaseQuantity>",
            "<cbc:PriceAmount currencyID=\"EUR\">5.00</cbc:PriceAmount>",
            "</Invoice>",
        ];
        let mut at = 0;
        for part in order {
            let found = out[at..]
                .find(part)
                .unwrap_or_else(|| panic!("{part} missing or out of order in\n{out}"));
            at += found + part.len();
        }
    }

    #[test]
    fn formats_parse() {
        for f in Format::ALL {
            assert_eq!(Format::parse(f.name()), Some(f));
        }
    }
}
