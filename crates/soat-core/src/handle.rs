//! Handles: the URL slug of a product, collection or page (`summer-sale`).

/// Longest handle.
pub const MAX_HANDLE: usize = 100;

/// 1 to 100 of `a-z`, `0-9` and `-`, not starting or ending with `-`
/// and without `--`.
pub fn is_handle(s: &str) -> bool {
    (1..=MAX_HANDLE).contains(&s.len())
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A handle made from a title: `Summer Sale!` becomes `summer-sale`.
/// Letters outside ASCII are dropped; an empty result is `item`.
pub fn handleize(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_HANDLE);
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "item".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles() {
        assert!(is_handle("summer-sale-2027"));
        for bad in ["", "-a", "a-", "a--b", "Summer", "a b", "ä"] {
            assert!(!is_handle(bad), "{bad}");
        }
        assert!(!is_handle(&"a".repeat(101)));
    }

    #[test]
    fn titles_become_handles() {
        assert_eq!(handleize("Summer Sale!"), "summer-sale");
        assert_eq!(handleize("  T-Shirts & Tops  "), "t-shirts-tops");
        assert_eq!(handleize("Über"), "ber");
        assert_eq!(handleize("!!!"), "item");
        assert!(is_handle(&handleize(&"ab ".repeat(80))));
    }
}
