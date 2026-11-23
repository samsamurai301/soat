//! A small, safe template language for emails and other shop texts.
//!
//! - `{{ order.number }}` writes a value (HTML-escaped when rendering HTML);
//!   a missing value writes nothing.
//! - `{{#each order.lines}} … {{/each}}` repeats for each element; inside,
//!   names are looked up on the element first, then outside it, and
//!   `{{ this }}` is the element itself.
//! - `{{#if order.discount_code}} … {{else}} … {{/if}}` shows the first part
//!   when the value is set: not null, false, `""`, `0` or an empty list.
//!
//! There are no other tags and no code: shops edit these texts, so a
//! template can only read the values it is given.

use std::fmt;

use serde_json::Value;

/// Longest template accepted, in bytes.
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
/// Deepest nesting of `each` and `if`.
pub const MAX_DEPTH: usize = 8;

/// Why a template was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Var(String),
    Each(String, Vec<Node>),
    If(String, Vec<Node>, Vec<Node>),
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template(Vec<Node>);

fn err(m: impl Into<String>) -> TemplateError {
    TemplateError(m.into())
}

fn is_path(p: &str) -> bool {
    p == "this"
        || (!p.is_empty()
            && p.split('.').all(|part| {
                !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }))
}

/// What closed a block being parsed.
enum End {
    Eof,
    Each,
    If,
    Else,
}

impl Template {
    /// Parses `source`.
    ///
    /// # Errors
    ///
    /// For an unclosed `{{`, an unknown or unbalanced tag, a bad name, a
    /// template over [`MAX_TEMPLATE_BYTES`] or nested deeper than
    /// [`MAX_DEPTH`].
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        if source.len() > MAX_TEMPLATE_BYTES {
            return Err(err("the template is too long"));
        }
        let mut rest = source;
        let (nodes, end) = parse_block(&mut rest, 0)?;
        match end {
            End::Eof => Ok(Self(nodes)),
            End::Each => Err(err("{{/each}} without {{#each}}")),
            End::If => Err(err("{{/if}} without {{#if}}")),
            End::Else => Err(err("{{else}} outside {{#if}}")),
        }
    }

    /// Writes the template out with `data`; `html` escapes values for HTML.
    pub fn render(&self, data: &Value, html: bool) -> String {
        let mut out = String::new();
        let mut scopes = vec![data];
        render_nodes(&self.0, &mut scopes, html, &mut out);
        out
    }
}

/// Parses and renders `source` in one go.
///
/// # Errors
///
/// When the template does not parse (see [`Template::parse`]).
pub fn render(source: &str, data: &Value, html: bool) -> Result<String, TemplateError> {
    Ok(Template::parse(source)?.render(data, html))
}

fn parse_block(rest: &mut &str, depth: usize) -> Result<(Vec<Node>, End), TemplateError> {
    if depth > MAX_DEPTH {
        return Err(err(format!("blocks nest deeper than {MAX_DEPTH}")));
    }
    let mut nodes = Vec::new();
    loop {
        let Some(open) = rest.find("{{") else {
            if !rest.is_empty() {
                nodes.push(Node::Text((*rest).to_owned()));
            }
            *rest = "";
            return Ok((nodes, End::Eof));
        };
        if open > 0 {
            nodes.push(Node::Text(rest[..open].to_owned()));
        }
        let after = &rest[open + 2..];
        let close = after.find("}}").ok_or_else(|| err("a {{ is not closed"))?;
        let tag = after[..close].trim();
        *rest = &after[close + 2..];
        if let Some(path) = tag.strip_prefix("#each ") {
            let path = path.trim();
            if !is_path(path) {
                return Err(err(format!("`{path}` is not a name")));
            }
            let (body, end) = parse_block(rest, depth + 1)?;
            if !matches!(end, End::Each) {
                return Err(err("{{#each}} is not closed with {{/each}}"));
            }
            nodes.push(Node::Each(path.to_owned(), body));
        } else if let Some(path) = tag.strip_prefix("#if ") {
            let path = path.trim();
            if !is_path(path) {
                return Err(err(format!("`{path}` is not a name")));
            }
            let (then, end) = parse_block(rest, depth + 1)?;
            let otherwise = match end {
                End::If => Vec::new(),
                End::Else => {
                    let (otherwise, end) = parse_block(rest, depth + 1)?;
                    if !matches!(end, End::If) {
                        return Err(err("{{#if}} is not closed with {{/if}}"));
                    }
                    otherwise
                }
                _ => return Err(err("{{#if}} is not closed with {{/if}}")),
            };
            nodes.push(Node::If(path.to_owned(), then, otherwise));
        } else if tag == "/each" {
            return Ok((nodes, End::Each));
        } else if tag == "/if" {
            return Ok((nodes, End::If));
        } else if tag == "else" {
            return Ok((nodes, End::Else));
        } else if is_path(tag) {
            nodes.push(Node::Var(tag.to_owned()));
        } else {
            return Err(err(format!("`{{{{{tag}}}}}` is not a known tag")));
        }
    }
}

fn lookup<'a>(scopes: &[&'a Value], path: &str) -> Option<&'a Value> {
    if path == "this" {
        return scopes.last().copied();
    }
    scopes.iter().rev().find_map(|scope| {
        path.split('.')
            .try_fold(*scope, |v, part| v.get(part))
            .filter(|v| !v.is_null())
    })
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

fn render_nodes(nodes: &[Node], scopes: &mut Vec<&Value>, html: bool, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Var(path) => {
                let text = match lookup(scopes, path) {
                    None | Some(Value::Null) => continue,
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(_) | Value::Object(_)) => continue,
                    Some(v) => v.to_string(),
                };
                if html {
                    escape_into(&text, out);
                } else {
                    out.push_str(&text);
                }
            }
            Node::Each(path, body) => {
                if let Some(Value::Array(items)) = lookup(scopes, path) {
                    for item in items {
                        scopes.push(item);
                        render_nodes(body, scopes, html, out);
                        scopes.pop();
                    }
                }
            }
            Node::If(path, then, otherwise) => {
                let branch = if truthy(lookup(scopes, path)) {
                    then
                } else {
                    otherwise
                };
                render_nodes(branch, scopes, html, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn writes_values_and_escapes_html() {
        let data = json!({ "shop": { "name": "Ben & Jerry's" }, "n": 1001, "ok": true });
        assert_eq!(
            render(
                "Order {{ n }} at {{shop.name}} {{ ok }}{{ missing }}",
                &data,
                false
            )
            .unwrap(),
            "Order 1001 at Ben & Jerry's true"
        );
        assert_eq!(
            render("<b>{{ shop.name }}</b>", &data, true).unwrap(),
            "<b>Ben &amp; Jerry&#39;s</b>"
        );
    }

    #[test]
    fn loops_and_conditions() {
        let data = json!({
            "currency": "EUR",
            "lines": [{ "title": "Tee", "quantity": 2 }, { "title": "<Book>", "quantity": 1 }],
            "code": "",
            "tags": ["a", "b"],
        });
        let t = "{{#each lines}}{{quantity}}x {{title}} ({{currency}});{{/each}}";
        assert_eq!(
            render(t, &data, true).unwrap(),
            "2x Tee (EUR);1x &lt;Book&gt; (EUR);"
        );
        assert_eq!(
            render("{{#each tags}}[{{this}}]{{/each}}", &data, false).unwrap(),
            "[a][b]"
        );
        assert_eq!(
            render(
                "{{#if code}}code{{else}}none{{/if}}{{#if lines}}!{{/if}}",
                &data,
                false
            )
            .unwrap(),
            "none!"
        );
    }

    #[test]
    fn rejects_bad_templates() {
        for bad in [
            "{{ order.number",
            "{{#each lines}}x",
            "{{/each}}",
            "{{#if a}}x{{/each}}",
            "{{else}}",
            "{{ a b }}",
            "{{> partial}}",
            "{{ a..b }}",
        ] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
        let deep = "{{#if a}}".repeat(MAX_DEPTH + 1) + &"{{/if}}".repeat(MAX_DEPTH + 1);
        assert!(Template::parse(&deep).is_err());
        assert!(Template::parse(&"x".repeat(MAX_TEMPLATE_BYTES + 1)).is_err());
    }
}
