//! Product feeds for shopping ads and catalogs: Google Merchant Center
//! (RSS 2.0 with the `g:` namespace) and Meta commerce catalogs (CSV).
//!
//! A feed is a list of [`FeedItem`]s, one per product without variants and
//! one per variant (grouped by `item_group_id`). [`problems`] names what
//! Google or Meta would reject or warn about, so staff can fix it first.

use crate::Currency;

/// Whether an item can be bought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    InStock,
    OutOfStock,
    /// Sold without stock, sent later.
    Backorder,
}

impl Availability {
    /// Google's word: `in_stock`, `out_of_stock` or `backorder`.
    pub fn google(self) -> &'static str {
        match self {
            Self::InStock => "in_stock",
            Self::OutOfStock => "out_of_stock",
            Self::Backorder => "backorder",
        }
    }

    /// Meta's words: `in stock`, `out of stock` or `available for order`.
    pub fn meta(self) -> &'static str {
        match self {
            Self::InStock => "in stock",
            Self::OutOfStock => "out of stock",
            Self::Backorder => "available for order",
        }
    }
}

/// One thing to sell in a feed.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedItem {
    /// The product's or variant's id.
    pub id: String,
    /// The product's id when the item is one of its variants.
    pub item_group_id: Option<String>,
    pub title: String,
    /// Plain text.
    pub description: String,
    /// The product page.
    pub link: String,
    pub image_link: Option<String>,
    pub additional_images: Vec<String>,
    pub availability: Availability,
    /// The regular price, in minor units of `currency`.
    pub price_minor: i64,
    /// What it sells for now, when lower than the regular price.
    pub sale_price_minor: Option<i64>,
    pub currency: Currency,
    pub brand: String,
    pub gtin: Option<String>,
    /// The maker's part number; the SKU by default.
    pub mpn: String,
    pub weight_grams: Option<i64>,
    /// Google product category, or a shop's own type (e.g. a tag).
    pub product_type: Option<String>,
}

/// Something a channel would reject (`error`) or show worse (`warning`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub item_id: String,
    /// `error` or `warning`.
    pub severity: &'static str,
    /// e.g. `missing_image`.
    pub code: &'static str,
}

/// What is wrong with `item` for Google and Meta.
///
/// ```
/// use soat_core::feeds::{problems, Availability, FeedItem};
/// use soat_core::Currency;
/// let item = FeedItem {
///     id: "prod_1".into(), item_group_id: None, title: "Mug".into(),
///     description: String::new(), link: "https://shop.example/products/mug".into(),
///     image_link: None, additional_images: vec![], availability: Availability::InStock,
///     price_minor: 1200, sale_price_minor: None, currency: Currency::parse("EUR").unwrap(),
///     brand: "Shop".into(), gtin: None, mpn: "MUG".into(), weight_grams: None,
///     product_type: None,
/// };
/// let codes: Vec<_> = problems(&item).into_iter().map(|p| p.code).collect();
/// assert_eq!(codes, ["missing_image", "missing_description", "missing_gtin"]);
/// ```
pub fn problems(item: &FeedItem) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut add = |severity, code| {
        out.push(Problem {
            item_id: item.id.clone(),
            severity,
            code,
        })
    };
    if item.image_link.is_none() {
        add("error", "missing_image");
    }
    if item.title.trim().is_empty() {
        add("error", "missing_title");
    } else if item.title.chars().count() > 150 {
        add("warning", "title_too_long");
    }
    if item.description.trim().is_empty() {
        add("error", "missing_description");
    }
    if item.price_minor <= 0 {
        add("error", "missing_price");
    }
    if !item.link.starts_with("https://") && !item.link.starts_with("http://") {
        add("error", "missing_link");
    }
    match &item.gtin {
        None => add("warning", "missing_gtin"),
        Some(g) if !gtin_ok(g) => add("error", "invalid_gtin"),
        Some(_) => {}
    }
    out
}

/// Whether `gtin` is a GTIN-8, -12, -13 or -14 with a right check digit.
///
/// ```
/// assert!(soat_core::feeds::gtin_ok("4006381333931"));
/// assert!(!soat_core::feeds::gtin_ok("4006381333932"));
/// ```
pub fn gtin_ok(gtin: &str) -> bool {
    let digits: Vec<u32> = gtin.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() != gtin.len() || ![8, 12, 13, 14].contains(&digits.len()) {
        return false;
    }
    let (body, check) = digits.split_at(digits.len() - 1);
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| if i % 2 == 0 { d * 3 } else { *d })
        .sum();
    (10 - sum % 10) % 10 == check[0]
}

fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 does not allow.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn cut(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// `items` as a Google Merchant Center RSS 2.0 feed of the shop called
/// `title` at `link`.
///
/// ```
/// let feed = soat_core::feeds::google_rss("Shop & Co", "https://shop.example", &[]);
/// assert!(feed.contains("<title>Shop &amp; Co</title>"));
/// assert!(feed.contains("xmlns:g=\"http://base.google.com/ns/1.0\""));
/// ```
pub fn google_rss(title: &str, link: &str, items: &[FeedItem]) -> String {
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:g=\"http://base.google.com/ns/1.0\">\n<channel>\n\
         <title>{}</title>\n<link>{}</link>\n<description>{}</description>\n",
        xml(title),
        xml(link),
        xml(title)
    );
    for i in items {
        let mut one = String::from("<item>");
        let mut tag = |name: &str, value: &str| {
            one.push_str(&format!("<g:{name}>{}</g:{name}>", xml(value)));
        };
        tag("id", &i.id);
        if let Some(g) = &i.item_group_id {
            tag("item_group_id", g);
        }
        tag("title", &cut(&i.title, 150));
        tag("description", &cut(&i.description, 5000));
        tag("link", &i.link);
        if let Some(img) = &i.image_link {
            tag("image_link", img);
        }
        for img in i.additional_images.iter().take(10) {
            tag("additional_image_link", img);
        }
        tag("availability", i.availability.google());
        tag("price", &i.currency.format(i.price_minor));
        if let Some(s) = i.sale_price_minor {
            tag("sale_price", &i.currency.format(s));
        }
        tag("condition", "new");
        tag("brand", &i.brand);
        match &i.gtin {
            Some(g) => tag("gtin", g),
            None => tag("identifier_exists", "no"),
        }
        tag("mpn", &i.mpn);
        if let Some(w) = i.weight_grams {
            tag("shipping_weight", &format!("{w} g"));
        }
        if let Some(t) = &i.product_type {
            tag("product_type", t);
        }
        one.push_str("</item>\n");
        out.push_str(&one);
    }
    out.push_str("</channel>\n</rss>\n");
    out
}

/// `items` as a Meta commerce catalog CSV.
///
/// ```
/// let csv = soat_core::feeds::meta_csv(&[]);
/// assert!(csv.starts_with("id,title,description,availability,condition,price,link,image_link,brand"));
/// ```
pub fn meta_csv(items: &[FeedItem]) -> String {
    let mut rows = vec![[
        "id",
        "title",
        "description",
        "availability",
        "condition",
        "price",
        "link",
        "image_link",
        "brand",
        "sale_price",
        "item_group_id",
        "gtin",
        "mpn",
        "additional_image_link",
        "shipping_weight",
        "product_type",
    ]
    .map(str::to_owned)
    .to_vec()];
    for i in items {
        rows.push(vec![
            i.id.clone(),
            cut(&i.title, 200),
            cut(&i.description, 9999),
            i.availability.meta().to_owned(),
            "new".to_owned(),
            i.currency.format(i.price_minor),
            i.link.clone(),
            i.image_link.clone().unwrap_or_default(),
            i.brand.clone(),
            i.sale_price_minor
                .map(|s| i.currency.format(s))
                .unwrap_or_default(),
            i.item_group_id.clone().unwrap_or_default(),
            i.gtin.clone().unwrap_or_default(),
            i.mpn.clone(),
            i.additional_images
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
            i.weight_grams.map(|w| format!("{w} g")).unwrap_or_default(),
            i.product_type.clone().unwrap_or_default(),
        ]);
    }
    crate::csv::write(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> FeedItem {
        FeedItem {
            id: "var_1".into(),
            item_group_id: Some("prod_1".into()),
            title: "Tee <M>".into(),
            description: "Soft, \"organic\" cotton".into(),
            link: "https://shop.example/products/tee".into(),
            image_link: Some("https://shop.example/files/a.jpg".into()),
            additional_images: vec!["https://shop.example/files/b.jpg".into()],
            availability: Availability::Backorder,
            price_minor: 2500,
            sale_price_minor: Some(1990),
            currency: Currency::parse("EUR").unwrap(),
            brand: "Acme".into(),
            gtin: Some("4006381333931".into()),
            mpn: "TEE-M".into(),
            weight_grams: Some(180),
            product_type: None,
        }
    }

    #[test]
    fn google_items_carry_every_field() {
        let feed = google_rss("Acme", "https://shop.example", &[item()]);
        for part in [
            "<g:id>var_1</g:id>",
            "<g:item_group_id>prod_1</g:item_group_id>",
            "<g:title>Tee &lt;M&gt;</g:title>",
            "<g:description>Soft, &quot;organic&quot; cotton</g:description>",
            "<g:availability>backorder</g:availability>",
            "<g:price>25.00 EUR</g:price>",
            "<g:sale_price>19.90 EUR</g:sale_price>",
            "<g:gtin>4006381333931</g:gtin>",
            "<g:shipping_weight>180 g</g:shipping_weight>",
            "<g:additional_image_link>https://shop.example/files/b.jpg</g:additional_image_link>",
        ] {
            assert!(feed.contains(part), "{part} in {feed}");
        }
        let no_gtin = FeedItem {
            gtin: None,
            ..item()
        };
        assert!(google_rss("A", "https://x", &[no_gtin]).contains("<g:identifier_exists>no"));
    }

    #[test]
    fn meta_rows_are_quoted() {
        let csv = meta_csv(&[item()]);
        let line = csv.lines().nth(1).unwrap();
        assert!(
            line.starts_with(
                "var_1,Tee <M>,\"Soft, \"\"organic\"\" cotton\",available for order,new,25.00 EUR"
            ),
            "{line}"
        );
    }

    #[test]
    fn problems_and_gtins() {
        assert!(problems(&item()).is_empty());
        let bad = FeedItem {
            gtin: Some("123".into()),
            price_minor: 0,
            title: "x".repeat(151),
            ..item()
        };
        let codes: Vec<_> = problems(&bad).into_iter().map(|p| p.code).collect();
        assert_eq!(codes, ["title_too_long", "missing_price", "invalid_gtin"]);
        assert!(gtin_ok("96385074"));
        assert!(gtin_ok("036000291452"));
        assert!(!gtin_ok("40063813339a1"));
    }
}
