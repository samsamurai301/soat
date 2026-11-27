//! SOAT's own import files, which other platforms' exports are turned
//! into ([`crate::shopify`], [`crate::woocommerce`], [`crate::shopware`]).

use crate::csv::Table;

/// The columns of SOAT's product file: a row per variant, product fields
/// on a product's first row.
pub const PRODUCT_COLUMNS: [&str; 21] = [
    "handle",
    "title",
    "description",
    "status",
    "tags",
    "sku",
    "price",
    "currency",
    "option1_name",
    "option1_value",
    "option2_name",
    "option2_value",
    "option3_name",
    "option3_value",
    "weight_grams",
    "tax_class",
    "stock",
    "seo_title",
    "seo_description",
    "images",
    "line",
];

/// One row of SOAT's product file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProductRow {
    pub handle: String,
    pub title: String,
    /// Markdown.
    pub description: String,
    /// `active` or `draft`; empty keeps it.
    pub status: String,
    /// Comma-separated.
    pub tags: String,
    pub sku: String,
    /// A decimal such as `29.00`.
    pub price: String,
    pub currency: String,
    /// Up to three (name, value) pairs.
    pub options: Vec<(String, String)>,
    pub weight_grams: String,
    /// `exempt`, or empty for the standard rate.
    pub tax_class: String,
    pub stock: String,
    pub seo_title: String,
    pub seo_description: String,
    /// Image URLs, separated by spaces, on a product's first row.
    pub images: String,
    /// Its line in the original export, for problems.
    pub line: usize,
}

impl ProductRow {
    fn cells(self) -> Vec<String> {
        let mut out = vec![
            self.handle,
            self.title,
            self.description,
            self.status,
            self.tags,
            self.sku,
            self.price,
            self.currency,
        ];
        for n in 0..3 {
            let (name, value) = self.options.get(n).cloned().unwrap_or_default();
            out.push(name);
            out.push(value);
        }
        out.extend([
            self.weight_grams,
            self.tax_class,
            self.stock,
            self.seo_title,
            self.seo_description,
            self.images,
            self.line.to_string(),
        ]);
        out
    }
}

/// SOAT's product file from `rows`.
pub fn products(rows: Vec<ProductRow>) -> Table {
    Table {
        headers: PRODUCT_COLUMNS.iter().map(|h| (*h).to_owned()).collect(),
        rows: rows.into_iter().map(ProductRow::cells).collect(),
    }
}

/// The columns of SOAT's customer file.
pub const CUSTOMER_COLUMNS: [&str; 7] = [
    "email",
    "first_name",
    "last_name",
    "phone",
    "accepts_marketing",
    "tags",
    "note",
];

/// A weight in `unit` (`kg`, `g`, `lbs`, `oz`) as whole grams; empty when
/// it is not a number.
pub fn grams(value: &str, unit: &str) -> String {
    let Ok(v) = value.trim().replace(',', ".").parse::<f64>() else {
        return String::new();
    };
    let factor = match unit {
        "g" => 1.0,
        "lbs" | "lb" => 453.592,
        "oz" => 28.3495,
        _ => 1000.0,
    };
    let g = (v * factor).round();
    if g.is_finite() && (0.0..1e9).contains(&g) {
        format!("{g:.0}")
    } else {
        String::new()
    }
}

/// A price as written in an export (`29`, `29,90`, `1.299,00`) as a
/// decimal with a point; as given when it cannot tell.
pub fn price(value: &str) -> String {
    let v = value.trim();
    match (v.rfind(','), v.rfind('.')) {
        // 1.299,00: points group thousands.
        (Some(c), Some(p)) if c > p => v.replace('.', "").replace(',', "."),
        // 1,299.00: commas group thousands.
        (Some(_), Some(_)) => v.replace(',', ""),
        (Some(_), None) => v.replace(',', "."),
        _ => v.to_owned(),
    }
}

/// The image URLs of an `images` cell, in order, each once: `https://`
/// (or `http://`) links separated by spaces or commas.
///
/// ```
/// let urls = soat_core::import::image_urls("https://a.test/1.jpg, https://a.test/2.png https://a.test/1.jpg x");
/// assert_eq!(urls, ["https://a.test/1.jpg", "https://a.test/2.png"]);
/// ```
pub fn image_urls(cell: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in cell.split([' ', ',', '\n', '\t']).map(str::trim) {
        let web = url.starts_with("https://") || url.starts_with("http://");
        if web && url.len() <= 2000 && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_and_prices_are_normalized() {
        assert_eq!(grams("0.25", "kg"), "250");
        assert_eq!(grams("1,5", "kg"), "1500");
        assert_eq!(grams("2", "lbs"), "907");
        assert_eq!(grams("", "kg"), "");
        assert_eq!(grams("-1", "kg"), "");
        assert_eq!(price("29,90"), "29.90");
        assert_eq!(price("1.299,00"), "1299.00");
        assert_eq!(price("1,299.00"), "1299.00");
        assert_eq!(price(" 29.00 "), "29.00");
    }

    #[test]
    fn rows_fill_every_column() {
        let t = products(vec![ProductRow {
            handle: "tee".into(),
            options: vec![("Size".into(), "S".into())],
            line: 2,
            ..ProductRow::default()
        }]);
        assert_eq!(t.rows[0].len(), PRODUCT_COLUMNS.len());
        assert_eq!(t.get(0, "option1_value"), "S");
        assert_eq!(t.get(0, "option2_name"), "");
        assert_eq!(t.get(0, "line"), "2");
    }
}
