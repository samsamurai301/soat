//! Postal addresses: of locations now, of customers and orders later.

/// An ISO 3166-1 alpha-2 country code such as `DE` (format only).
pub fn is_country_code(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_codes() {
        assert!(is_country_code("DE"));
        assert!(!is_country_code("de"));
        assert!(!is_country_code("DEU"));
    }
}
