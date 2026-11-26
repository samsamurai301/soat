//! Content: the blocks a page is built from, and menus.
//!
//! A page is a list of blocks, each a JSON object with a `type`:
//!
//! | type | fields |
//! |---|---|
//! | `heading` | `text`, `level` (1 to 6, default 2) |
//! | `text` | `body` (Markdown) |
//! | `image` | `url` or `file_id`, `alt`, `link` |
//! | `button` | `label`, `url` |
//! | `products` | `collection_id` or `product_ids` (at most 50), `limit` (at most 50) |
//! | `video` | `url` (https) |
//! | `spacer` | `size`: `small`, `medium` or `large` |
//! | `columns` | `columns`: 2 to 4 lists of blocks (no columns inside) |
//!
//! Unknown fields are refused, so a typo is an error, not a silent miss.
//! A menu is a tree of items (`title`, `url`, `items`), three levels deep.

use serde_json::{Map, Value};

/// Most blocks on a page, counting those inside columns.
pub const MAX_BLOCKS: usize = 100;
/// Most items in a menu, at every level.
pub const MAX_MENU_ITEMS: usize = 200;
/// How deep a menu goes.
pub const MAX_MENU_DEPTH: usize = 3;

/// A link a shop may put on a page: a path on the shop (`/products/x`),
/// `http(s)://…`, `mailto:` or `tel:`. Never `javascript:` or `//host`.
pub fn is_link(s: &str) -> bool {
    let safe =
        !s.is_empty() && s.len() <= 2000 && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    let lower = s.to_ascii_lowercase();
    safe && ((s.starts_with('/') && !s.starts_with("//"))
        || lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:"))
}

struct Check<'a> {
    at: String,
    map: &'a Map<String, Value>,
}

impl<'a> Check<'a> {
    fn only(&self, allowed: &[&str]) -> Result<(), String> {
        match self
            .map
            .keys()
            .find(|k| *k != "type" && !allowed.contains(&k.as_str()))
        {
            Some(k) => Err(format!("{}.{k} is not a field of this block", self.at)),
            None => Ok(()),
        }
    }

    fn text(&self, name: &str, required: bool, max: usize) -> Result<Option<&'a str>, String> {
        match self.map.get(name) {
            None | Some(Value::Null) if !required => Ok(None),
            Some(Value::String(s)) if !(required && s.trim().is_empty()) => {
                if s.chars().count() > max {
                    Err(format!(
                        "{}.{name} is longer than {max} characters",
                        self.at
                    ))
                } else {
                    Ok(Some(s))
                }
            }
            _ => Err(format!("{}.{name} must be text", self.at)),
        }
    }

    fn link(&self, name: &str, required: bool) -> Result<Option<&'a str>, String> {
        let v = self.text(name, required, 2000)?;
        match v {
            Some(s) if !is_link(s) => Err(format!(
                "{}.{name} must be a path on the shop or an http(s), mailto or tel link",
                self.at
            )),
            other => Ok(other),
        }
    }

    fn number(&self, name: &str, min: i64, max: i64) -> Result<(), String> {
        match self.map.get(name) {
            None | Some(Value::Null) => Ok(()),
            Some(v) if v.as_i64().is_some_and(|n| (min..=max).contains(&n)) => Ok(()),
            _ => Err(format!(
                "{}.{name} must be a whole number from {min} to {max}",
                self.at
            )),
        }
    }
}

fn block(at: String, v: &Value, nested: bool, count: &mut usize) -> Result<(), String> {
    *count += 1;
    if *count > MAX_BLOCKS {
        return Err(format!("a page has at most {MAX_BLOCKS} blocks"));
    }
    let Some(map) = v.as_object() else {
        return Err(format!("{at} must be an object"));
    };
    let c = Check {
        at: at.clone(),
        map,
    };
    let kind = map.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "heading" => {
            c.only(&["text", "level"])?;
            c.text("text", true, 200)?;
            c.number("level", 1, 6)
        }
        "text" => {
            c.only(&["body"])?;
            c.text("body", true, 20_000).map(drop)
        }
        "image" => {
            c.only(&["url", "file_id", "alt", "link"])?;
            let url = c.link("url", false)?;
            let file = c.text("file_id", false, 64)?;
            if url.is_none() == file.is_none() {
                return Err(format!("{at} needs a url or a file_id, not both"));
            }
            c.text("alt", false, 300)?;
            c.link("link", false).map(drop)
        }
        "button" => {
            c.only(&["label", "url"])?;
            c.text("label", true, 100)?;
            c.link("url", true).map(drop)
        }
        "products" => {
            c.only(&["collection_id", "product_ids", "limit", "title"])?;
            c.text("title", false, 200)?;
            c.number("limit", 1, 50)?;
            let collection = c.text("collection_id", false, 64)?;
            match map.get("product_ids") {
                None | Some(Value::Null) if collection.is_some() => Ok(()),
                Some(Value::Array(ids))
                    if collection.is_none()
                        && !ids.is_empty()
                        && ids.len() <= 50
                        && ids
                            .iter()
                            .all(|i| i.as_str().is_some_and(|s| s.len() <= 64)) =>
                {
                    Ok(())
                }
                _ => Err(format!(
                    "{at} needs a collection_id or 1 to 50 product_ids, not both"
                )),
            }
        }
        "video" => {
            c.only(&["url", "title"])?;
            c.text("title", false, 200)?;
            match c.text("url", true, 2000)? {
                Some(u) if u.to_ascii_lowercase().starts_with("https://") && is_link(u) => Ok(()),
                _ => Err(format!("{at}.url must be an https link")),
            }
        }
        "spacer" => {
            c.only(&["size"])?;
            match c.text("size", false, 10)? {
                None | Some("small" | "medium" | "large") => Ok(()),
                Some(_) => Err(format!("{at}.size must be small, medium or large")),
            }
        }
        "columns" if !nested => {
            c.only(&["columns"])?;
            let Some(cols) = map
                .get("columns")
                .and_then(Value::as_array)
                .filter(|c| (2..=4).contains(&c.len()))
            else {
                return Err(format!("{at}.columns must be 2 to 4 lists of blocks"));
            };
            for (i, col) in cols.iter().enumerate() {
                let Some(blocks) = col.as_array() else {
                    return Err(format!("{at}.columns[{i}] must be a list of blocks"));
                };
                for (j, b) in blocks.iter().enumerate() {
                    block(format!("{at}.columns[{i}][{j}]"), b, true, count)?;
                }
            }
            Ok(())
        }
        "columns" => Err(format!("{at}: columns cannot hold columns")),
        "" => Err(format!("{at}.type is missing")),
        other => Err(format!("{at}.type {other} is not a block type")),
    }
}

/// Checks a page's blocks. The error names the first bad field, e.g.
/// `blocks[2].url must be …`.
pub fn check_blocks(blocks: &[Value]) -> Result<(), String> {
    let mut count = 0;
    for (i, b) in blocks.iter().enumerate() {
        block(format!("blocks[{i}]"), b, false, &mut count)?;
    }
    Ok(())
}

fn menu_items(at: &str, items: &[Value], depth: usize, count: &mut usize) -> Result<(), String> {
    if depth > MAX_MENU_DEPTH && !items.is_empty() {
        return Err(format!("a menu is at most {MAX_MENU_DEPTH} levels deep"));
    }
    for (i, item) in items.iter().enumerate() {
        let at = format!("{at}[{i}]");
        *count += 1;
        if *count > MAX_MENU_ITEMS {
            return Err(format!("a menu has at most {MAX_MENU_ITEMS} items"));
        }
        let Some(map) = item.as_object() else {
            return Err(format!("{at} must be an object"));
        };
        let c = Check {
            at: at.clone(),
            map,
        };
        c.only(&["title", "url", "items"])?;
        c.text("title", true, 100)?;
        c.link("url", true)?;
        match map.get("items") {
            None | Some(Value::Null) => {}
            Some(Value::Array(children)) => {
                menu_items(&format!("{at}.items"), children, depth + 1, count)?;
            }
            Some(_) => return Err(format!("{at}.items must be a list")),
        }
    }
    Ok(())
}

/// Checks a menu's items.
pub fn check_menu(items: &[Value]) -> Result<(), String> {
    menu_items("items", items, 1, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocks(v: Value) -> Result<(), String> {
        check_blocks(v.as_array().unwrap())
    }

    #[test]
    fn links() {
        for ok in [
            "/about",
            "https://x.example/a?b=1",
            "mailto:a@b.c",
            "tel:+491234",
        ] {
            assert!(is_link(ok), "{ok}");
        }
        for bad in [
            "",
            "//evil.example",
            "javascript:alert(1)",
            "JAVASCRIPT:x",
            "a b",
            "about",
        ] {
            assert!(!is_link(bad), "{bad}");
        }
    }

    #[test]
    fn good_pages_pass() {
        blocks(json!([
            { "type": "heading", "text": "Hello", "level": 1 },
            { "type": "text", "body": "We *love* shoes." },
            { "type": "image", "file_id": "file_1", "alt": "a shoe", "link": "/products/shoe" },
            { "type": "button", "label": "Shop", "url": "/collections/all" },
            { "type": "products", "collection_id": "col_1", "limit": 8 },
            { "type": "video", "url": "https://www.youtube.com/watch?v=x" },
            { "type": "spacer", "size": "large" },
            { "type": "columns", "columns": [[{ "type": "text", "body": "a" }], []] }
        ]))
        .unwrap();
    }

    #[test]
    fn bad_blocks_say_where() {
        let cases = [
            (
                json!([{ "type": "heading" }]),
                "blocks[0].text must be text",
            ),
            (
                json!([{ "type": "heading", "text": "x", "level": 7 }]),
                "blocks[0].level must be a whole number from 1 to 6",
            ),
            (
                json!([{ "type": "text", "body": "x", "bdy": 1 }]),
                "blocks[0].bdy is not a field of this block",
            ),
            (
                json!([{ "type": "button", "label": "x", "url": "javascript:alert(1)" }]),
                "blocks[0].url must be a path on the shop or an http(s), mailto or tel link",
            ),
            (
                json!([{ "type": "image", "url": "/a.png", "file_id": "f" }]),
                "blocks[0] needs a url or a file_id, not both",
            ),
            (
                json!([{ "type": "products" }]),
                "blocks[0] needs a collection_id or 1 to 50 product_ids, not both",
            ),
            (
                json!([{ "type": "video", "url": "http://x.example" }]),
                "blocks[0].url must be an https link",
            ),
            (
                json!([{ "type": "marquee" }]),
                "blocks[0].type marquee is not a block type",
            ),
            (json!([1]), "blocks[0] must be an object"),
            (
                json!([{ "type": "columns", "columns": [[{ "type": "columns", "columns": [[], []] }], []] }]),
                "blocks[0].columns[0][0]: columns cannot hold columns",
            ),
            (
                json!([{ "type": "columns", "columns": [[]] }]),
                "blocks[0].columns must be 2 to 4 lists of blocks",
            ),
        ];
        for (v, want) in cases {
            assert_eq!(blocks(v).unwrap_err(), want);
        }
        let many: Vec<Value> = (0..101).map(|_| json!({ "type": "spacer" })).collect();
        assert_eq!(
            check_blocks(&many).unwrap_err(),
            "a page has at most 100 blocks"
        );
    }

    #[test]
    fn menus() {
        let item = |items: Value| json!({ "title": "A", "url": "/a", "items": items });
        check_menu(&[item(json!([item(json!([item(json!([]))]))]))]).unwrap();
        assert_eq!(
            check_menu(&[item(json!([item(json!([item(json!([item(json!(
                null
            ))]))]))]))])
            .unwrap_err(),
            "a menu is at most 3 levels deep"
        );
        assert_eq!(
            check_menu(&[json!({ "title": "", "url": "/a" })]).unwrap_err(),
            "items[0].title must be text"
        );
        assert_eq!(
            check_menu(&[
                json!({ "title": "x", "url": "/a", "items": [{ "title": "y", "url": "//x" }] })
            ])
            .unwrap_err(),
            "items[0].items[0].url must be a path on the shop or an http(s), mailto or tel link"
        );
    }
}
