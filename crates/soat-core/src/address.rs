//! Postal addresses: of locations now, of customers and orders later.

/// An ISO 3166-1 alpha-2 country code such as `DE` (format only).
pub fn is_country_code(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// Most countries in one list (there are about 250).
pub const MAX_COUNTRIES: usize = 300;

/// A list of country codes, uppercased, sorted and deduplicated; the error
/// names the first bad code.
pub fn country_list(raw: &[String]) -> Result<Vec<String>, String> {
    if raw.len() > MAX_COUNTRIES {
        return Err(format!("at most {MAX_COUNTRIES} countries"));
    }
    let mut out = Vec::with_capacity(raw.len());
    for c in raw {
        let c = c.trim().to_ascii_uppercase();
        if !is_country_code(&c) {
            return Err(format!("{c} is not an ISO 3166-1 alpha-2 country code"));
        }
        out.push(c);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_lists() {
        let l = |v: &[&str]| country_list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(l(&["fr", " DE", "FR"]), Ok(vec!["DE".into(), "FR".into()]));
        assert!(l(&["DEU"]).is_err());
        assert_eq!(l(&[]), Ok(vec![]));
    }

    #[test]
    fn country_codes() {
        assert!(is_country_code("DE"));
        assert!(!is_country_code("de"));
        assert!(!is_country_code("DEU"));
    }
}
