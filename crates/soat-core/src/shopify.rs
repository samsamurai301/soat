//! Shopify's CSV exports read as SOAT's import files: its product export
//! (Products > Export) and customer export (Customers > Export), as they
//! are. Product descriptions turn from HTML into SOAT's Markdown.

use crate::csv::Table;

/// Whether `t` is Shopify's product export.
pub fn is_products(t: &Table) -> bool {
    t.column("handle").is_some() && t.column("variant price").is_some()
}

/// Whether `t` is Shopify's customer export.
pub fn is_customers(t: &Table) -> bool {
    t.column("email").is_some() && t.column("first name").is_some()
}

/// The product export as SOAT's product file (see the admin's import):
/// a row per variant, with `line` its line in the export. Rows that only
/// add an image are left out, a
/// product's only `Default Title` variant becomes a product without
/// options, and a variant that is not taxable is `exempt`.
pub fn products(t: &Table) -> Table {
    let headers: Vec<String> = [
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
        "line",
    ]
    .iter()
    .map(|h| (*h).to_owned())
    .collect();
    let mut rows = Vec::new();
    let mut last_handle = String::new();
    for r in 0..t.rows.len() {
        let handle = t.get(r, "handle").to_owned();
        let variant = !t.get(r, "variant price").is_empty()
            || !t.get(r, "variant sku").is_empty()
            || !t.get(r, "option1 value").is_empty();
        if handle.is_empty() || !variant {
            continue;
        }
        let first = handle != last_handle;
        last_handle.clone_from(&handle);
        let status = match t.get(r, "status").to_lowercase().as_str() {
            "" => match t.get(r, "published").to_lowercase().as_str() {
                "false" => "draft".to_owned(),
                "" if !first => String::new(),
                _ => "active".to_owned(),
            },
            s => s.to_owned(),
        };
        let default =
            t.get(r, "option1 name") == "Title" && t.get(r, "option1 value") == "Default Title";
        let mut row = vec![
            handle,
            t.get(r, "title").to_owned(),
            to_markdown(t.get(r, "body (html)")),
            if first { status } else { String::new() },
            t.get(r, "tags").to_owned(),
            t.get(r, "variant sku").to_owned(),
            t.get(r, "variant price").to_owned(),
            String::new(),
        ];
        for n in 1..=3 {
            if default {
                row.push(String::new());
                row.push(String::new());
            } else {
                row.push(t.get(r, &format!("option{n} name")).to_owned());
                row.push(t.get(r, &format!("option{n} value")).to_owned());
            }
        }
        row.push(match t.get(r, "variant grams") {
            g if first => g.split('.').next().unwrap_or_default().to_owned(),
            _ => String::new(),
        });
        row.push(match t.get(r, "variant taxable").to_lowercase().as_str() {
            "false" if first => "exempt".to_owned(),
            _ => String::new(),
        });
        // Shopify counts stock only when it tracks it.
        row.push(
            if t.get(r, "variant inventory tracker").is_empty()
                && t.column("variant inventory tracker").is_some()
            {
                String::new()
            } else {
                t.get(r, "variant inventory qty").to_owned()
            },
        );
        row.push(t.get(r, "seo title").to_owned());
        row.push(t.get(r, "seo description").to_owned());
        row.push((r + 2).to_string());
        rows.push(row);
    }
    Table { headers, rows }
}

/// The customer export as SOAT's customer file.
pub fn customers(t: &Table) -> Table {
    let headers: Vec<String> = [
        "email",
        "first_name",
        "last_name",
        "phone",
        "accepts_marketing",
        "tags",
        "note",
    ]
    .iter()
    .map(|h| (*h).to_owned())
    .collect();
    let marketing = if t.column("accepts email marketing").is_some() {
        "accepts email marketing"
    } else {
        "accepts marketing"
    };
    let rows = (0..t.rows.len())
        .map(|r| {
            let phone = match t.get(r, "phone") {
                "" => t.get(r, "default address phone"),
                p => p,
            };
            vec![
                t.get(r, "email").to_owned(),
                t.get(r, "first name").to_owned(),
                t.get(r, "last name").to_owned(),
                // Shopify writes numbers as text with a leading quote.
                phone.trim_start_matches('\'').to_owned(),
                t.get(r, marketing).to_owned(),
                t.get(r, "tags").to_owned(),
                t.get(r, "note").to_owned(),
            ]
        })
        .collect();
    Table { headers, rows }
}

/// Shopify's description HTML as Markdown: paragraphs, line breaks,
/// headings, lists, bold, italics and links stay; every other tag is
/// dropped and its text kept.
pub fn to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut list: Vec<Option<u32>> = Vec::new();
    let mut link: Option<String> = None;
    while let Some(start) = rest.find('<') {
        text(&mut out, &rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            text(&mut out, &rest[start..]);
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("br", false) => out.push('\n'),
            ("p" | "div" | "ul" | "ol" | "table" | "tr", _) => block(&mut out),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                block(&mut out);
                let level = name[1..].parse::<usize>().unwrap_or(2).max(2);
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => block(&mut out),
            ("strong" | "b", _) => out.push_str("**"),
            ("em" | "i", _) => out.push('*'),
            ("li", false) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                match list.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("- "),
                }
            }
            ("a", false) => {
                link = attribute(tag, "href");
                if link.is_some() {
                    out.push('[');
                }
            }
            ("a", true) => {
                if let Some(href) = link.take() {
                    out.push_str(&format!("]({href})"));
                }
            }
            _ => {}
        }
        match (name.as_str(), closing) {
            ("ul", false) => list.push(None),
            ("ol", false) => list.push(Some(1)),
            ("ul" | "ol", true) => {
                list.pop();
            }
            _ => {}
        }
    }
    text(&mut out, rest);
    // At most one blank line, none at the ends, no trailing spaces.
    let mut clean = String::new();
    let mut blank = 0;
    for line in out.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !clean.is_empty() {
            clean.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        clean.push_str(line.trim_start());
    }
    clean
}

fn block(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
    }
}

fn text(out: &mut String, s: &str) {
    let s = s
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&rsquo;", "’")
        .replace("&amp;", "&");
    // HTML's own line breaks are spaces.
    let s = s.replace(['\n', '\r', '\t'], " ");
    if out.ends_with([' ', '\n']) || out.is_empty() {
        out.push_str(s.trim_start());
    } else {
        out.push_str(&s);
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let value = match rest.chars().next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next()?,
        _ => rest.split([' ', '>']).next()?,
    };
    Some(value.to_owned()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_markdown() {
        let html = "<p>Soft <strong>organic</strong> cotton.<br>Made in <em>Portugal</em>.</p>\n\
            <h3>Care</h3><ul><li>Wash cold</li><li>Line dry</li></ul>\
            <ol><li>One</li><li>Two</li></ol>\
            <p>See <a href=\"/pages/sizes\">sizes</a> &amp; more.<span style=\"x\"></span></p>";
        assert_eq!(
            to_markdown(html),
            "Soft **organic** cotton.\nMade in *Portugal*.\n\n### Care\n\n- Wash cold\n- Line dry\n\n1. One\n2. Two\n\nSee [sizes](/pages/sizes) & more."
        );
        assert_eq!(to_markdown("Just text"), "Just text");
        assert_eq!(to_markdown(""), "");
        assert_eq!(to_markdown("<h1>Big</h1>"), "## Big");
    }

    const EXPORT: &str = "Handle,Title,Body (HTML),Vendor,Tags,Published,Option1 Name,Option1 Value,Option2 Name,Option2 Value,Variant SKU,Variant Grams,Variant Inventory Tracker,Variant Inventory Qty,Variant Price,Variant Taxable,Image Src,SEO Title,Status\n\
        tee,Tee,<p>Soft</p>,Acme,\"tops, cotton\",TRUE,Size,S,Color,Red,TEE-S-R,180.0,shopify,4,29.00,TRUE,https://cdn/x.jpg,Organic tee,active\n\
        tee,,,,,,,M,,Red,TEE-M-R,180.0,shopify,0,29.00,TRUE,,,\n\
        tee,,,,,,,,,,,,,,,,https://cdn/y.jpg,,\n\
        card,Gift card,,Acme,,FALSE,Title,Default Title,,,CARD,0,,,25.00,FALSE,,,\n";

    #[test]
    fn reads_the_product_export() {
        let t = Table::parse(EXPORT).unwrap();
        assert!(is_products(&t) && !is_customers(&t));
        let p = products(&t);
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.get(0, "description"), "Soft");
        assert_eq!(p.get(0, "status"), "active");
        assert_eq!(p.get(0, "option2_name"), "Color");
        assert_eq!(p.get(0, "weight_grams"), "180");
        assert_eq!(p.get(0, "stock"), "4");
        assert_eq!(p.get(0, "seo_title"), "Organic tee");
        assert_eq!(p.get(1, "option1_value"), "M");
        assert_eq!(p.get(1, "status"), "");
        assert_eq!(p.get(2, "option1_name"), "");
        assert_eq!(p.get(2, "status"), "draft");
        assert_eq!(p.get(2, "tax_class"), "exempt");
        // Not tracked by Shopify: no count.
        assert_eq!(p.get(2, "stock"), "");
        assert_eq!(p.get(2, "line"), "5");
    }

    #[test]
    fn reads_the_customer_export() {
        let t = Table::parse(
            "First Name,Last Name,Email,Accepts Email Marketing,Default Address Phone,Phone,Tags,Note\n\
             Ada,Lovelace,ada@example.com,yes,'+4930123,,vip,Likes tea\n",
        )
        .unwrap();
        assert!(is_customers(&t) && !is_products(&t));
        let c = customers(&t);
        assert_eq!(c.get(0, "first_name"), "Ada");
        assert_eq!(c.get(0, "phone"), "+4930123");
        assert_eq!(c.get(0, "accepts_marketing"), "yes");
        assert_eq!(c.get(0, "note"), "Likes tea");
    }
}
