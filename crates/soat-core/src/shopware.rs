//! Shopware 6's CSV exports (Settings > Import/Export, the default
//! product and customer profiles) read as SOAT's import files. A parent
//! product's variants become variants with one option, `Variant`, named
//! by the variant's own name or number: the default profile lists a
//! variant's options only by id. Prices are taken gross; descriptions
//! turn from HTML into Markdown; images are not imported.

use crate::csv::Table;
use crate::handle::handleize;
use crate::import::{price, products as file, ProductRow, CUSTOMER_COLUMNS};
use crate::shopify::to_markdown;

/// Whether `t` is Shopware's product export.
pub fn is_products(t: &Table) -> bool {
    t.column("product_number").is_some()
        && (t.column("price_gross").is_some() || t.column("price_net").is_some())
}

/// Whether `t` is Shopware's customer export.
pub fn is_customers(t: &Table) -> bool {
    t.column("customer_number").is_some() && t.column("email").is_some()
}

fn active(t: &Table, r: usize) -> String {
    match t.get(r, "active").to_lowercase().as_str() {
        "0" | "false" => "draft",
        _ => "active",
    }
    .to_owned()
}

fn amount(t: &Table, r: usize) -> String {
    price(match t.get(r, "price_gross") {
        "" => t.get(r, "price_net"),
        g => g,
    })
}

/// The export as SOAT's product file, a row per variant.
pub fn products(t: &Table) -> Table {
    let mut out = Vec::new();
    for r in 0..t.rows.len() {
        if !t.get(r, "parent_id").is_empty() {
            continue;
        }
        let name = t.get(r, "name");
        let product = ProductRow {
            handle: handleize(name),
            title: name.to_owned(),
            description: to_markdown(t.get(r, "description")),
            status: active(t, r),
            line: r + 2,
            ..ProductRow::default()
        };
        let id = t.get(r, "id");
        let children: Vec<usize> = (0..t.rows.len())
            .filter(|&c| !id.is_empty() && t.get(c, "parent_id") == id)
            .collect();
        if children.is_empty() {
            out.push(ProductRow {
                sku: t.get(r, "product_number").to_owned(),
                price: amount(t, r),
                stock: t.get(r, "stock").to_owned(),
                ..product
            });
            continue;
        }
        for (i, &c) in children.iter().enumerate() {
            let label = match t.get(c, "name") {
                "" => t.get(c, "product_number"),
                n => n,
            };
            // A variant without its own price inherits the parent's.
            let price = match amount(t, c) {
                p if p.is_empty() => amount(t, r),
                p => p,
            };
            let base = if i == 0 {
                product.clone()
            } else {
                ProductRow {
                    handle: product.handle.clone(),
                    ..ProductRow::default()
                }
            };
            out.push(ProductRow {
                sku: t.get(c, "product_number").to_owned(),
                price,
                stock: t.get(c, "stock").to_owned(),
                options: vec![("Variant".to_owned(), label.to_owned())],
                line: c + 2,
                ..base
            });
        }
    }
    file(out)
}

/// The customer export as SOAT's customer file.
pub fn customers(t: &Table) -> Table {
    let pick = |r: usize, names: &[&str]| {
        names
            .iter()
            .map(|n| t.get(r, n))
            .find(|v| !v.is_empty())
            .unwrap_or_default()
            .to_owned()
    };
    let rows = (0..t.rows.len())
        .map(|r| {
            vec![
                t.get(r, "email").to_owned(),
                pick(r, &["first_name", "firstname"]),
                pick(r, &["last_name", "lastname"]),
                pick(r, &["phone_number", "default_billing_address_phone_number"]),
                pick(r, &["newsletter"]),
                pick(r, &["tags"]),
                String::new(),
            ]
        })
        .collect();
    Table {
        headers: CUSTOMER_COLUMNS.iter().map(|h| (*h).to_owned()).collect(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = "id;parent_id;product_number;active;stock;name;description;price_net;price_gross;tax_rate;cover_media_url\n\
        a1;;SW-MUG;1;7;Enamel Mug;<p>A mug.</p>;10,08;12,00;19;https://x/m.jpg\n\
        b1;;SW-TEE;1;0;Organic Tee;<p>Soft</p>;24,37;29,00;19;\n\
        b2;b1;SW-TEE-S;1;4;Organic Tee S;;;;19;\n\
        b3;b1;SW-TEE-M;0;2;;;;31,00;19;\n";

    #[test]
    fn reads_the_product_export() {
        let t = Table::parse(EXPORT).unwrap();
        assert!(is_products(&t) && !is_customers(&t));
        let p = products(&t);
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.get(0, "handle"), "enamel-mug");
        assert_eq!(p.get(0, "price"), "12.00");
        assert_eq!(p.get(0, "description"), "A mug.");
        assert_eq!(p.get(0, "stock"), "7");
        assert_eq!(p.get(1, "handle"), "organic-tee");
        assert_eq!(p.get(1, "title"), "Organic Tee");
        assert_eq!(p.get(1, "sku"), "SW-TEE-S");
        assert_eq!(p.get(1, "price"), "29.00", "inherited");
        assert_eq!(p.get(1, "option1_name"), "Variant");
        assert_eq!(p.get(1, "option1_value"), "Organic Tee S");
        assert_eq!(p.get(2, "title"), "");
        assert_eq!(p.get(2, "option1_value"), "SW-TEE-M");
        assert_eq!(p.get(2, "price"), "31.00");
        assert_eq!(p.get(2, "line"), "5");
    }

    #[test]
    fn reads_the_customer_export() {
        let t = Table::parse(
            "id;customer_number;email;first_name;last_name;newsletter\n\
             c1;10001;ada@example.com;Ada;Lovelace;1\n",
        )
        .unwrap();
        assert!(is_customers(&t) && !is_products(&t));
        let c = customers(&t);
        assert_eq!(c.get(0, "email"), "ada@example.com");
        assert_eq!(c.get(0, "last_name"), "Lovelace");
        assert_eq!(c.get(0, "accepts_marketing"), "1");
    }
}
