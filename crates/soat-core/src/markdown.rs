//! A small, safe Markdown for shop texts: product descriptions, page text.
//!
//! Paragraphs, `#` headings, `-`/`*` and `1.` lists, `>` quotes, and inline
//! `**strong**`, `*emphasis*`, `` `code` `` and `[links](/path)`. Every
//! character of the input is HTML-escaped first, so no HTML gets through;
//! links must pass [`is_link`](crate::content::is_link).

use crate::content::is_link;

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Wraps text between `marker`s in `tag`, on already-escaped text. With
/// `hold`, each wrapped piece is set aside and replaced by a placeholder,
/// so later passes leave it alone.
fn pairs(s: &str, marker: &str, tag: &str, held: &mut Option<&mut Vec<String>>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        match after.find(marker) {
            Some(end) if end > 0 && !after[..end].starts_with(' ') => {
                out.push_str(&rest[..start]);
                let piece = format!("<{tag}>{}</{tag}>", &after[..end]);
                match held {
                    Some(h) => {
                        out.push_str(&format!("\u{0}{}\u{0}", h.len()));
                        h.push(piece);
                    }
                    None => out.push_str(&piece),
                }
                rest = &after[end + marker.len()..];
            }
            _ => {
                out.push_str(&rest[..start + marker.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn links(s: &str, held: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let found = after.find("](").and_then(|mid| {
            let url_part = &after[mid + 2..];
            url_part
                .find(')')
                .map(|close| (mid, &url_part[..close], close))
        });
        match found {
            Some((mid, url, close)) if is_link(&unescape(url)) && mid > 0 => {
                out.push_str(&rest[..open]);
                let external = url.starts_with("http");
                let text = pairs(&after[..mid], "**", "strong", &mut None);
                let text = pairs(&text, "*", "em", &mut None);
                out.push_str(&format!("\u{0}{}\u{0}", held.len()));
                held.push(format!(
                    "<a href=\"{url}\"{}>{text}</a>",
                    if external {
                        " rel=\"noopener nofollow\""
                    } else {
                        ""
                    },
                ));
                rest = &after[mid + 2 + close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Inline markup of one line.
fn inline(line: &str) -> String {
    let clean: String = line.trim().chars().filter(|c| !c.is_control()).collect();
    let mut held = Vec::new();
    let s = escape(&clean);
    let s = pairs(&s, "`", "code", &mut Some(&mut held));
    let s = links(&s, &mut held);
    let s = pairs(&s, "**", "strong", &mut None);
    let s = pairs(&s, "*", "em", &mut None);
    let mut out = String::new();
    for (i, part) in s.split('\u{0}').enumerate() {
        if i % 2 == 1 {
            if let Some(piece) = part.parse::<usize>().ok().and_then(|n| held.get(n)) {
                out.push_str(piece);
                continue;
            }
        }
        out.push_str(part);
    }
    out
}

enum Open {
    None,
    Para(Vec<String>),
    List(&'static str, Vec<String>),
    Quote(Vec<String>),
}

fn close(open: &mut Open, out: &mut String) {
    match std::mem::replace(open, Open::None) {
        Open::None => {}
        Open::Para(lines) => out.push_str(&format!("<p>{}</p>\n", lines.join(" "))),
        Open::List(tag, items) => {
            out.push_str(&format!("<{tag}>"));
            for i in items {
                out.push_str(&format!("<li>{i}</li>"));
            }
            out.push_str(&format!("</{tag}>\n"));
        }
        Open::Quote(lines) => {
            out.push_str(&format!(
                "<blockquote><p>{}</p></blockquote>\n",
                lines.join(" ")
            ));
        }
    }
}

fn ordered_item(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && digits < 10)
        .then(|| line[digits..].strip_prefix(". "))
        .flatten()
}

/// Renders `source` as HTML.
pub fn to_html(source: &str) -> String {
    let mut out = String::new();
    let mut open = Open::None;
    for raw in source.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            close(&mut open, &mut out);
            continue;
        }
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            close(&mut open, &mut out);
            out.push_str(&format!(
                "<h{hashes}>{}</h{hashes}>\n",
                inline(&t[hashes..])
            ));
            continue;
        }
        let bullet = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "));
        let (tag, item) = match (bullet, ordered_item(t)) {
            (Some(i), _) => ("ul", Some(i)),
            (None, Some(i)) => ("ol", Some(i)),
            _ => ("", None),
        };
        if let Some(item) = item {
            match &mut open {
                Open::List(t2, items) if *t2 == tag => items.push(inline(item)),
                _ => {
                    close(&mut open, &mut out);
                    open = Open::List(if tag == "ul" { "ul" } else { "ol" }, vec![inline(item)]);
                }
            }
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            match &mut open {
                Open::Quote(lines) => lines.push(inline(q)),
                _ => {
                    close(&mut open, &mut out);
                    open = Open::Quote(vec![inline(q)]);
                }
            }
            continue;
        }
        match &mut open {
            Open::Para(lines) => lines.push(inline(t)),
            _ => {
                close(&mut open, &mut out);
                open = Open::Para(vec![inline(t)]);
            }
        }
    }
    close(&mut open, &mut out);
    out
}

/// `source` as plain text on one line, at most `max` characters (cut at a
/// word, with `…`): for meta descriptions and structured data.
pub fn to_text(source: &str, max: usize) -> String {
    let html = to_html(source);
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let text = unescape(&words.join(" "));
    let text = text
        .replace(" .", ".")
        .replace(" ,", ",")
        .replace(" !", "!")
        .replace(" ?", "?");
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', '.', ';', ':']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        assert_eq!(
            to_html("# Shoes\n\nLight and\n*grippy*.\n\n- one\n- **two**\n\n1. first\n2. second\n\n> said"),
            "<h1>Shoes</h1>\n<p>Light and <em>grippy</em>.</p>\n<ul><li>one</li><li><strong>two</strong></li></ul>\n\
             <ol><li>first</li><li>second</li></ol>\n<blockquote><p>said</p></blockquote>\n"
        );
        assert_eq!(to_html(""), "");
        assert_eq!(to_html("#nospace"), "<p>#nospace</p>\n");
    }

    #[test]
    fn inline_marks() {
        assert_eq!(
            to_html("see [our story](/pages/about) and [x](https://a.example/?a=1&b=2) `a*b*`"),
            "<p>see <a href=\"/pages/about\">our story</a> and \
             <a href=\"https://a.example/?a=1&amp;b=2\" rel=\"noopener nofollow\">x</a> <code>a*b*</code></p>\n"
        );
        assert_eq!(to_html("2 * 3 * 4"), "<p>2 * 3 * 4</p>\n");
    }

    #[test]
    fn plain_text() {
        assert_eq!(
            to_text("# Shoes\n\nLight and *grippy*. [More](/a) & <b>", 200),
            "Shoes Light and grippy. More & <b>"
        );
        assert_eq!(to_text("one two three four", 12), "one two…");
        assert_eq!(to_text("", 10), "");
    }

    #[test]
    fn nothing_unsafe_gets_through() {
        assert_eq!(
            to_html("<script>alert(1)</script> [x](javascript:alert(1)) [y](\" onclick=\"x)"),
            "<p>&lt;script&gt;alert(1)&lt;/script&gt; [x](javascript:alert(1)) [y](&quot; onclick=&quot;x)</p>\n"
        );
        assert_eq!(
            to_html("<img src=x onerror=alert(1)>"),
            "<p>&lt;img src=x onerror=alert(1)&gt;</p>\n"
        );
    }
}
