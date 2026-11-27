//! WooCommerce's product CSV export (Products > Export) read as SOAT's
//! product file. Simple products become products without options;
//! variable products take their variations as variants, with the
//! variations' attributes as options. Grouped and external products are
//! left out: SOAT does not sell them. Descriptions turn from HTML into
//! Markdown; the images' links go along for the import to fetch.

use crate::csv::Table;
use crate::handle::handleize;
use crate::import::{grams, price, products as file, ProductRow};
use crate::shopify::to_markdown;

/// Whether `t` is WooCommerce's product export.
pub fn is_products(t: &Table) -> bool {
    t.column("type").is_some() && t.column("regular price").is_some() && t.column("name").is_some()
}

/// The weight column and its unit, e.g. (`weight (kg)`, `kg`).
fn weight_column(t: &Table) -> Option<(String, String)> {
    t.headers.iter().find_map(|h| {
        let unit = h.strip_prefix("weight (")?.strip_suffix(')')?;
        Some((h.clone(), unit.to_owned()))
    })
}

/// The product-level fields of row `r`.
fn product_fields(t: &Table, r: usize) -> ProductRow {
    let description = match t.get(r, "description") {
        "" => t.get(r, "short description"),
        d => d,
    };
    ProductRow {
        handle: handleize(t.get(r, "name")),
        title: t.get(r, "name").to_owned(),
        description: to_markdown(description),
        status: match t.get(r, "published") {
            "1" => "active",
            _ => "draft",
        }
        .to_owned(),
        tags: t.get(r, "tags").to_owned(),
        images: crate::import::image_urls(t.get(r, "images")).join(" "),
        line: r + 2,
        ..ProductRow::default()
    }
}

/// The variant-level fields of row `r` into `row`.
fn variant_fields(t: &Table, r: usize, row: &mut ProductRow) {
    row.sku = t.get(r, "sku").to_owned();
    // The sale price is what buyers pay now.
    row.price = price(match t.get(r, "sale price") {
        "" => t.get(r, "regular price"),
        s => s,
    });
    if let Some((column, unit)) = weight_column(t) {
        row.weight_grams = grams(t.get(r, &column), &unit);
    }
    if t.get(r, "tax status") == "none" {
        "exempt".clone_into(&mut row.tax_class);
    }
    row.stock = t.get(r, "stock").to_owned();
    row.line = r + 2;
}

/// The variation's options: each attribute with one value.
fn options(t: &Table, r: usize) -> Vec<(String, String)> {
    (1..)
        .map_while(|n| {
            t.column(&format!("attribute {n} name")).map(|_| {
                (
                    t.get(r, &format!("attribute {n} name")),
                    t.get(r, &format!("attribute {n} value(s)")),
                )
            })
        })
        .filter(|(name, value)| !name.is_empty() && !value.is_empty())
        .take(3)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The export as SOAT's product file, a row per variant.
pub fn products(t: &Table) -> Table {
    let kind = |r: usize| t.get(r, "type").to_lowercase();
    // Which row a variation's `parent` cell names: `id:123` or a SKU.
    let parent_of = |r: usize| -> Option<usize> {
        let p = t.get(r, "parent");
        if p.is_empty() {
            return None;
        }
        (0..t.rows.len()).find(|&q| match p.strip_prefix("id:") {
            Some(id) => t.get(q, "id") == id,
            None => t.get(q, "sku") == p,
        })
    };
    let mut out = Vec::new();
    for r in 0..t.rows.len() {
        let k = kind(r);
        if k.contains("variation") {
            continue;
        }
        if k.contains("variable") {
            let mut first = true;
            for v in (0..t.rows.len())
                .filter(|&v| kind(v).contains("variation") && parent_of(v) == Some(r))
            {
                let mut row = if first {
                    product_fields(t, r)
                } else {
                    ProductRow {
                        handle: handleize(t.get(r, "name")),
                        ..ProductRow::default()
                    }
                };
                variant_fields(t, v, &mut row);
                row.options = options(t, v);
                out.push(row);
                first = false;
            }
            if first {
                // No variations: the importer says what is missing.
                out.push(product_fields(t, r));
            }
        } else if k.contains("simple") || k.is_empty() {
            let mut row = product_fields(t, r);
            variant_fields(t, r, &mut row);
            out.push(row);
        }
    }
    file(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = "ID,Type,SKU,Name,Published,Short description,Description,Tax status,In stock?,Stock,Weight (kg),Sale price,Regular price,Tags,Images,Parent,Attribute 1 name,Attribute 1 value(s),Attribute 2 name,Attribute 2 value(s)\n\
        10,simple,MUG,Enamel Mug,1,Short,<p>A <strong>mug</strong>.</p>,taxable,1,7,0.35,,\"12,00\",\"kitchen, gift\",https://x/mug.jpg,,,,,\n\
        11,variable,TEE,Organic Tee,1,,<p>Soft</p>,taxable,1,,,,,tops,,,Size,\"S, M\",Color,Red\n\
        12,variation,TEE-S,Organic Tee - S,1,,,taxable,1,4,0.18,25.00,29.00,,,id:11,Size,S,Color,Red\n\
        13,\"variation, virtual\",TEE-M,Organic Tee - M,1,,,taxable,1,0,0.2,,29.00,,,TEE,Size,M,Color,Red\n\
        14,simple,CARD,Gift Card,0,,,none,1,,,,25,,,,,,,\n\
        15,external,EXT,Elsewhere,1,,,,,,,,9,,,,,,,\n";

    #[test]
    fn reads_the_product_export() {
        let t = Table::parse(EXPORT).unwrap();
        assert!(is_products(&t));
        assert!(!crate::shopify::is_products(&t));
        let p = products(&t);
        assert_eq!(p.rows.len(), 4);
        assert_eq!(p.get(0, "handle"), "enamel-mug");
        assert_eq!(p.get(0, "description"), "A **mug**.");
        assert_eq!(p.get(0, "price"), "12.00");
        assert_eq!(p.get(0, "weight_grams"), "350");
        assert_eq!(p.get(0, "stock"), "7");
        assert_eq!(p.get(0, "images"), "https://x/mug.jpg");
        assert_eq!(p.get(0, "option1_name"), "");
        assert_eq!(p.get(1, "handle"), "organic-tee");
        assert_eq!(p.get(1, "title"), "Organic Tee");
        assert_eq!(p.get(1, "status"), "active");
        assert_eq!(p.get(1, "sku"), "TEE-S");
        assert_eq!(p.get(1, "price"), "25.00", "the sale price");
        assert_eq!(p.get(1, "option1_name"), "Size");
        assert_eq!(p.get(1, "option1_value"), "S");
        assert_eq!(p.get(1, "option2_value"), "Red");
        assert_eq!(p.get(1, "line"), "4");
        // Later variants carry only their own fields; a parent named by SKU.
        assert_eq!(p.get(2, "handle"), "organic-tee");
        assert_eq!(p.get(2, "title"), "");
        assert_eq!(p.get(2, "sku"), "TEE-M");
        assert_eq!(p.get(2, "stock"), "0");
        assert_eq!(p.get(3, "status"), "draft");
        assert_eq!(p.get(3, "tax_class"), "exempt");
    }
}
