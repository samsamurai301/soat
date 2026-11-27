//! Nonces for the Content Security Policy.
//!
//! Page templates are tagged once, when they are parsed: every `<script>`
//! and `<style>` element written in the template's own source gets
//! `nonce="<placeholder>"` ([`tag`]). The placeholder is random per process
//! and never reaches a browser: for each HTML response the server puts a
//! fresh nonce in its place ([`fill`]) and names that nonce in the
//! `Content-Security-Policy` header. So the scripts the theme and the admin
//! wrote run, and a script that reached a page any other way (content,
//! a customer's input) has no nonce and is refused.

use std::sync::OnceLock;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The stand-in for the nonce in tagged templates: random per process.
pub fn placeholder() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).expect("the OS random source is available");
        format!("soat-csp-{}", hex(&random))
    })
}

/// A fresh nonce for one response: 128 random bits.
pub fn nonce() -> String {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).expect("the OS random source is available");
    hex(&random)
}

/// `source` with `nonce="<placeholder>"` on every `<script>` and `<style>`
/// start tag that has no nonce yet.
pub fn tag(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut out = String::with_capacity(source.len() + 64);
    let mut done = 0;
    let mut from = 0;
    while let Some(at) = lower[from..].find('<').map(|i| from + i) {
        from = at + 1;
        let name_len = if lower[at + 1..].starts_with("script") {
            6
        } else if lower[at + 1..].starts_with("style") {
            5
        } else {
            continue;
        };
        let after = at + 1 + name_len;
        if !lower[after..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>') {
            continue;
        }
        let end = lower[after..].find('>').map_or(lower.len(), |i| after + i);
        if lower[after..end].contains("nonce=") {
            continue;
        }
        out.push_str(&source[done..after]);
        out.push_str(" nonce=\"");
        out.push_str(placeholder());
        out.push('"');
        done = after;
    }
    out.push_str(&source[done..]);
    out
}

/// `html` with the placeholder replaced by `nonce`, or `None` when it
/// holds no placeholder.
pub fn fill(html: &str, nonce: &str) -> Option<String> {
    html.contains(placeholder())
        .then(|| html.replace(placeholder(), nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_styles_get_the_placeholder() {
        let p = placeholder();
        assert_eq!(
            tag(r#"<script src="/a.js" defer></script><SCRIPT>x()</SCRIPT><style>a{}</style>"#),
            format!(
                r#"<script nonce="{p}" src="/a.js" defer></script><SCRIPT nonce="{p}">x()</SCRIPT><style nonce="{p}">a{{}}</style>"#
            )
        );
    }

    #[test]
    fn other_tags_and_tagged_ones_are_left_alone() {
        let s = r#"<scripts><styled><p>script</p><script nonce="x">y</script>"#;
        assert_eq!(tag(s), s);
    }

    #[test]
    fn fill_swaps_the_placeholder_for_the_nonce() {
        let html = tag("<script>x()</script>");
        let filled = fill(&html, "abc").unwrap();
        assert_eq!(filled, r#"<script nonce="abc">x()</script>"#);
        assert!(fill("<p>no scripts</p>", "abc").is_none());
        assert_ne!(nonce(), nonce());
        assert_eq!(nonce().len(), 32);
    }
}
