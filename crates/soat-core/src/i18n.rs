//! Languages: the codes shops publish their storefront in.

/// A language code as used in storefront addresses: `de`, or a regional
/// variant like `de-at` (lowercase).
pub fn is_language_code(s: &str) -> bool {
    let b = s.as_bytes();
    let two = |x: &[u8]| x.len() == 2 && x.iter().all(u8::is_ascii_lowercase);
    match b.len() {
        2 => two(b),
        5 => two(&b[..2]) && b[2] == b'-' && two(&b[3..]),
        _ => false,
    }
}

/// The best of `published` for an `Accept-Language` header: an exact
/// match first, then the same base language (`de-AT` → `de`), by the
/// header's order.
pub fn negotiate<'a>(accept: &str, published: &'a [String]) -> Option<&'a String> {
    let mut wanted: Vec<(String, f32)> = accept
        .split(',')
        .filter_map(|part| {
            let mut bits = part.trim().split(';');
            let tag = bits.next()?.trim().to_ascii_lowercase();
            let q = bits
                .find_map(|b| b.trim().strip_prefix("q="))
                .and_then(|q| q.parse::<f32>().ok())
                .unwrap_or(1.0);
            (!tag.is_empty() && tag != "*" && q > 0.0).then_some((tag, q))
        })
        .take(20)
        .collect();
    wanted.sort_by(|a, b| b.1.total_cmp(&a.1));
    wanted.iter().find_map(|(tag, _)| {
        published.iter().find(|p| *p == tag).or_else(|| {
            let base = tag.split('-').next().unwrap_or_default();
            published.iter().find(|p| *p == base)
        })
    })
}

/// A language's own name, e.g. `Deutsch` for `de` (the base language for
/// `de-at`); the code itself for one not listed.
pub fn language_name(code: &str) -> String {
    let base = code.split('-').next().unwrap_or_default();
    let name = match base {
        "bg" => "Български",
        "cs" => "Čeština",
        "da" => "Dansk",
        "de" => "Deutsch",
        "el" => "Ελληνικά",
        "en" => "English",
        "es" => "Español",
        "et" => "Eesti",
        "fi" => "Suomi",
        "fr" => "Français",
        "ga" => "Gaeilge",
        "hr" => "Hrvatski",
        "hu" => "Magyar",
        "is" => "Íslenska",
        "it" => "Italiano",
        "ja" => "日本語",
        "lt" => "Lietuvių",
        "lv" => "Latviešu",
        "mt" => "Malti",
        "nb" | "no" => "Norsk",
        "nl" => "Nederlands",
        "pl" => "Polski",
        "pt" => "Português",
        "ro" => "Română",
        "sk" => "Slovenčina",
        "sl" => "Slovenščina",
        "sv" => "Svenska",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "zh" => "中文",
        _ => return code.to_owned(),
    };
    if base == code {
        name.to_owned()
    } else {
        format!("{name} ({})", code[3..].to_ascii_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for ok in ["de", "en", "de-at", "pt-br"] {
            assert!(is_language_code(ok), "{ok}");
        }
        for bad in ["", "d", "deu", "DE", "de-AT", "de_at", "de-", "api"] {
            assert!(!is_language_code(bad), "{bad}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(language_name("de"), "Deutsch");
        assert_eq!(language_name("de-at"), "Deutsch (AT)");
        assert_eq!(language_name("xx"), "xx");
    }

    #[test]
    fn negotiates() {
        let p = vec!["en".to_owned(), "de".to_owned(), "fr-ca".to_owned()];
        assert_eq!(
            negotiate("de-AT,de;q=0.9,en;q=0.8", &p).map(String::as_str),
            Some("de")
        );
        assert_eq!(negotiate("fr-CA", &p).map(String::as_str), Some("fr-ca"));
        assert_eq!(
            negotiate("es, en;q=0.1", &p).map(String::as_str),
            Some("en")
        );
        assert_eq!(
            negotiate("en;q=0.2, de;q=0.9", &p).map(String::as_str),
            Some("de")
        );
        assert_eq!(negotiate("ja", &p), None);
        assert_eq!(negotiate("", &p), None);
    }
}
