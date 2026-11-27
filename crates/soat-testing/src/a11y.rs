//! Accessibility checks of rendered HTML: the WCAG 2.1 success criteria a
//! page's markup alone can break, so tests catch them on every page.
//!
//! It is no replacement for testing with a screen reader, nor for what
//! needs a browser (contrast, focus order, motion), but it fails a page
//! with an unlabeled field, an image without `alt`, a link or button
//! without a name, a skipped heading level, a duplicate `id`, a missing
//! `lang` or `<title>`, a positive `tabindex` or a viewport that stops
//! zooming.
//!
//! ```
//! use soat_testing::a11y::check;
//! let page = r#"<!doctype html><html lang="en"><head><title>Shop</title></head>
//! <body><h1>Shop</h1><img src="a.png"><label>Email <input name="email"></label></body></html>"#;
//! let rules: Vec<_> = check(page).into_iter().map(|i| i.rule).collect();
//! assert_eq!(rules, ["1.1.1 image-alt"]);
//! ```

use std::collections::{HashMap, HashSet};

/// One thing a page gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The WCAG success criterion and a short name, e.g. `1.1.1 image-alt`.
    pub rule: &'static str,
    /// The element, as a short tag, e.g. `<input name="email">`.
    pub element: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rule, self.element)
    }
}

#[derive(Debug)]
enum Token {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
    },
    End(String),
    Text(String),
}

const VOID: [&str; 13] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn attrs_of(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name = raw[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let s = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = decode(&raw[s..i]);
                i += 1;
            } else {
                let s = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = decode(&raw[s..i]);
            }
        }
        out.push((name, value));
    }
    out
}

fn tokens(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Token::Text(decode(rest)));
            break;
        };
        if lt > 0 {
            out.push(Token::Text(decode(&rest[..lt])));
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            break;
        };
        let inner = &rest[1..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Token::End(name.trim().to_ascii_lowercase()));
            continue;
        }
        let split = inner
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = inner[..split].to_ascii_lowercase();
        if name.is_empty() {
            out.push(Token::Text("<".into()));
            continue;
        }
        let attrs = attrs_of(&inner[split..]);
        out.push(Token::Start {
            name: name.clone(),
            attrs,
        });
        // Raw text: skip to the end tag.
        if name == "script" || name == "style" || name == "template" {
            let close = format!("</{name}");
            match rest.to_ascii_lowercase().find(&close) {
                Some(e) => rest = &rest[e..],
                None => break,
            }
        }
    }
    out
}

fn get<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn named_by_aria(attrs: &[(String, String)]) -> bool {
    ["aria-label", "aria-labelledby", "title"]
        .iter()
        .any(|a| get(attrs, a).is_some_and(|v| !v.trim().is_empty()))
}

fn describe(name: &str, attrs: &[(String, String)]) -> String {
    let mut s = format!("<{name}");
    let short = |v: &str| v.chars().take(60).collect::<String>();
    if let Some((key, v)) = ["id", "name", "href", "action", "src", "class"]
        .iter()
        .find_map(|k| get(attrs, k).map(|v| (k, v)))
    {
        s.push_str(&format!(" {key}=\"{}\"", short(v)));
    }
    if let Some(v) = get(attrs, "type") {
        s.push_str(&format!(" type=\"{}\"", short(v)));
    }
    s.push('>');
    s
}

/// An element whose accessible name comes from its content.
struct Naming {
    tag: String,
    element: String,
    text: String,
    named: bool,
}

/// What `html` gets wrong, in page order (label references at the end).
pub fn check(html: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut push = |rule: &'static str, element: String| issues.push(Issue { rule, element });
    let toks = tokens(html);

    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut label_for: Vec<String> = Vec::new();
    let mut unlabeled: Vec<(String, String)> = Vec::new();
    let mut labels_open = 0usize;
    let mut naming: Vec<Naming> = Vec::new();
    let mut last_heading = 0u8;
    let mut title: Option<String> = None;
    let mut in_title = false;
    let mut is_document = false;
    let mut hidden_depth: Vec<String> = Vec::new();

    for t in &toks {
        match t {
            Token::Start { name, attrs } => {
                let name = name.as_str();
                if !hidden_depth.is_empty() {
                    if !VOID.contains(&name) {
                        hidden_depth.push(name.to_owned());
                    }
                    continue;
                }
                if let Some(id) = get(attrs, "id") {
                    *ids.entry(id.to_owned()).or_default() += 1;
                }
                if get(attrs, "hidden").is_some() || get(attrs, "aria-hidden") == Some("true") {
                    if !VOID.contains(&name) {
                        hidden_depth.push(name.to_owned());
                    }
                    continue;
                }
                if get(attrs, "tabindex")
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .is_some_and(|v| v > 0)
                {
                    push("2.4.3 tabindex", describe(name, attrs));
                }
                match name {
                    "html" => {
                        is_document = true;
                        if get(attrs, "lang").is_none_or(|l| l.trim().is_empty()) {
                            push("3.1.1 html-lang", "<html>".into());
                        }
                    }
                    "title" => {
                        in_title = true;
                        title.get_or_insert_with(String::new);
                    }
                    "meta" if get(attrs, "name") == Some("viewport") => {
                        let c = get(attrs, "content")
                            .unwrap_or_default()
                            .replace(' ', "")
                            .to_ascii_lowercase();
                        let stops = c.contains("user-scalable=no")
                            || c.contains("user-scalable=0")
                            || c.split(',').any(|p| {
                                p.strip_prefix("maximum-scale=")
                                    .and_then(|v| v.parse::<f32>().ok())
                                    .is_some_and(|v| v < 2.0)
                            });
                        if stops {
                            push("1.4.4 viewport-zoom", "<meta name=\"viewport\">".into());
                        }
                    }
                    "img" => {
                        if get(attrs, "alt").is_none() && get(attrs, "role") != Some("presentation")
                        {
                            push("1.1.1 image-alt", describe(name, attrs));
                        }
                        let alt = get(attrs, "alt").unwrap_or_default();
                        for n in &mut naming {
                            n.text.push_str(alt);
                        }
                    }
                    "iframe" => {
                        if get(attrs, "title").is_none_or(|v| v.trim().is_empty()) {
                            push("4.1.2 frame-title", describe(name, attrs));
                        }
                    }
                    "label" => {
                        labels_open += 1;
                        if let Some(f) = get(attrs, "for") {
                            label_for.push(f.to_owned());
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let level = name.as_bytes()[1] - b'0';
                        if last_heading != 0 && level > last_heading + 1 {
                            push(
                                "1.3.1 heading-order",
                                format!("<{name}> after <h{last_heading}>"),
                            );
                        }
                        last_heading = level;
                    }
                    "input" | "select" | "textarea" => {
                        let kind = get(attrs, "type").unwrap_or("text").to_ascii_lowercase();
                        if matches!(kind.as_str(), "hidden" | "submit" | "reset" | "button") {
                            continue;
                        }
                        if kind == "image" {
                            if get(attrs, "alt").is_none_or(|v| v.trim().is_empty()) {
                                push("1.1.1 image-alt", describe(name, attrs));
                            }
                            continue;
                        }
                        if labels_open > 0 || named_by_aria(attrs) {
                            continue;
                        }
                        match get(attrs, "id") {
                            Some(id) => unlabeled.push((id.to_owned(), describe(name, attrs))),
                            None => push("1.3.1 label", describe(name, attrs)),
                        }
                    }
                    "a" if get(attrs, "href").is_some() => naming.push(Naming {
                        tag: "a".into(),
                        element: describe(name, attrs),
                        text: String::new(),
                        named: named_by_aria(attrs),
                    }),
                    "button" => naming.push(Naming {
                        tag: "button".into(),
                        element: describe(name, attrs),
                        text: String::new(),
                        named: named_by_aria(attrs),
                    }),
                    "svg" if named_by_aria(attrs) => {
                        for n in &mut naming {
                            n.text.push('·');
                        }
                    }
                    _ => {}
                }
            }
            Token::End(name) => {
                if let Some(top) = hidden_depth.last() {
                    if top == name {
                        hidden_depth.pop();
                    }
                    continue;
                }
                match name.as_str() {
                    "title" => in_title = false,
                    "label" => labels_open = labels_open.saturating_sub(1),
                    "a" | "button" => {
                        if let Some(pos) = naming.iter().rposition(|n| &n.tag == name) {
                            let n = naming.remove(pos);
                            if !n.named && n.text.trim().is_empty() {
                                let rule = if n.tag == "a" {
                                    "2.4.4 link-name"
                                } else {
                                    "4.1.2 button-name"
                                };
                                push(rule, n.element);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Token::Text(text) => {
                if !hidden_depth.is_empty() {
                    continue;
                }
                if in_title {
                    if let Some(t) = title.as_mut() {
                        t.push_str(text);
                    }
                }
                for n in &mut naming {
                    n.text.push_str(text);
                }
            }
        }
    }
    if is_document && title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        push("2.4.2 page-title", "<title>".into());
    }
    let labelled: HashSet<&str> = label_for.iter().map(String::as_str).collect();
    for (id, element) in unlabeled {
        if !labelled.contains(id.as_str()) {
            push("1.3.1 label", element);
        }
    }
    let mut dupes: Vec<&String> = ids
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(i, _)| i)
        .collect();
    dupes.sort();
    for id in dupes {
        push("4.1.1 duplicate-id", format!("id=\"{id}\""));
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(html: &str) -> Vec<&'static str> {
        check(html).into_iter().map(|i| i.rule).collect()
    }

    const OK: &str = r#"<!doctype html><html lang="de"><head><title>Kaffee</title>
<meta name="viewport" content="width=device-width, initial-scale=1"><script>if (a < b) { x = "<a href=x></a>"; }</script></head>
<body><h1>Kaffee</h1><!-- <img src=x> --><h2>Bohnen</h2><h3>Mehr</h3><h2>Wieder</h2>
<img src="logo.png" alt=""><a href="/"><img src="l.png" alt="Home"></a>
<a href="/cart" aria-label="Cart"><svg></svg></a>
<label for="q">Search</label><input id="q" name="q"><input type="hidden" name="t">
<label>Note <textarea name="note"></textarea></label><select aria-label="Size"><option>S</option></select>
<button>Save &amp; go</button><button aria-label="Close">×</button><input type="submit" value="Go">
<div hidden><input name="ghost"></div>
</body></html>"#;

    #[test]
    fn a_good_page_passes() {
        assert_eq!(rules(OK), Vec::<&str>::new());
    }

    #[test]
    fn each_rule() {
        assert_eq!(
            rules("<html><title></title></html>"),
            ["3.1.1 html-lang", "2.4.2 page-title"]
        );
        assert_eq!(rules(r#"<img src="x.png">"#), ["1.1.1 image-alt"]);
        assert_eq!(rules(r#"<input name="email">"#), ["1.3.1 label"]);
        assert_eq!(
            rules(r#"<input id="e"><label for="other">E</label>"#),
            ["1.3.1 label"]
        );
        assert_eq!(rules(r#"<a href="/x"></a>"#), ["2.4.4 link-name"]);
        assert_eq!(
            rules(r#"<a href="/x"><img src="i.png" alt=""></a>"#),
            ["2.4.4 link-name"]
        );
        assert_eq!(rules("<button> </button>"), ["4.1.2 button-name"]);
        assert_eq!(rules("<h1>a</h1><h3>b</h3>"), ["1.3.1 heading-order"]);
        assert_eq!(
            rules(r#"<p id="a"></p><p id="a"></p>"#),
            ["4.1.1 duplicate-id"]
        );
        assert_eq!(rules(r#"<div tabindex="2">x</div>"#), ["2.4.3 tabindex"]);
        assert_eq!(
            rules(r#"<iframe src="/x"></iframe>"#),
            ["4.1.2 frame-title"]
        );
        assert_eq!(
            rules(r#"<meta name="viewport" content="width=device-width, maximum-scale=1">"#),
            ["1.4.4 viewport-zoom"]
        );
        assert_eq!(
            rules(r#"<input type="image" src="go.png">"#),
            ["1.1.1 image-alt"]
        );
    }

    #[test]
    fn issues_name_their_element() {
        let i = &check(r#"<form action="/s"><input type="email" name="email"></form>"#)[0];
        assert_eq!(
            i.to_string(),
            "1.3.1 label: <input name=\"email\" type=\"email\">"
        );
    }
}
