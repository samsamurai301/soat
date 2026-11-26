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

/// English names of the countries shops sell to most, by code.
const COUNTRY_NAMES: &[(&str, &str)] = &[
    ("AD", "Andorra"),
    ("AL", "Albania"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("BA", "Bosnia and Herzegovina"),
    ("BE", "Belgium"),
    ("BG", "Bulgaria"),
    ("BR", "Brazil"),
    ("CA", "Canada"),
    ("CH", "Switzerland"),
    ("CN", "China"),
    ("CY", "Cyprus"),
    ("CZ", "Czechia"),
    ("DE", "Germany"),
    ("DK", "Denmark"),
    ("EE", "Estonia"),
    ("ES", "Spain"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("GB", "United Kingdom"),
    ("GR", "Greece"),
    ("HR", "Croatia"),
    ("HU", "Hungary"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IN", "India"),
    ("IS", "Iceland"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("KR", "South Korea"),
    ("LI", "Liechtenstein"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("LV", "Latvia"),
    ("MC", "Monaco"),
    ("MD", "Moldova"),
    ("ME", "Montenegro"),
    ("MK", "North Macedonia"),
    ("MT", "Malta"),
    ("MX", "Mexico"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NZ", "New Zealand"),
    ("PL", "Poland"),
    ("PT", "Portugal"),
    ("RO", "Romania"),
    ("RS", "Serbia"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("SI", "Slovenia"),
    ("SK", "Slovakia"),
    ("SM", "San Marino"),
    ("TR", "Türkiye"),
    ("UA", "Ukraine"),
    ("US", "United States"),
    ("VA", "Vatican City"),
    ("ZA", "South Africa"),
];

/// The common countries as `(code, English name)`, sorted by code.
pub fn country_names() -> &'static [(&'static str, &'static str)] {
    COUNTRY_NAMES
}

/// The English name of country `code`, if it is one of the common ones;
/// otherwise the code itself is the best label.
pub fn country_name(code: &str) -> Option<&'static str> {
    COUNTRY_NAMES
        .binary_search_by(|(c, _)| (*c).cmp(code))
        .ok()
        .map(|i| COUNTRY_NAMES[i].1)
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
        assert_eq!(country_name("DE"), Some("Germany"));
        assert_eq!(country_name("ZZ"), None);
    }

    #[test]
    fn country_codes() {
        assert!(is_country_code("DE"));
        assert!(!is_country_code("de"));
        assert!(!is_country_code("DEU"));
    }
}
