//! Tax: rates in basis points, tax classes, the EU VAT rates preset and
//! VAT number formats. Nothing here knows about shops or storage.

/// The class every product has unless set otherwise.
pub const STANDARD: &str = "standard";
/// A class that is never taxed, whatever the zone says.
pub const EXEMPT: &str = "exempt";

/// Highest rate: 100% (10 000 basis points).
pub const MAX_RATE_BPS: i64 = 10_000;

/// A tax class code such as `standard`, `reduced` or `books`: 1 to 32 of
/// `a-z`, `0-9`, `-` and `_`, starting with a letter.
pub fn is_tax_class(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Tax on `amount_minor` at `rate_bps` (1900 = 19%), rounded half away from
/// zero to the minor unit. With `inclusive`, the amount already holds the
/// tax (a gross price) and the tax is taken out of it; otherwise it is
/// added on top of a net price.
pub fn tax_on(amount_minor: i64, rate_bps: i64, inclusive: bool) -> i64 {
    let amount = i128::from(amount_minor);
    let rate = i128::from(rate_bps);
    let (num, den) = if inclusive {
        (amount * rate, 10_000 + rate)
    } else {
        (amount * rate, 10_000)
    };
    let half = den / 2;
    let rounded = if num >= 0 {
        (num + half) / den
    } else {
        (num - half) / den
    };
    rounded as i64
}

/// The EU member states (ISO 3166-1 alpha-2).
pub const EU_COUNTRIES: [&str; 27] = [
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
];

/// Whether `country` is an EU member state.
pub fn is_eu(country: &str) -> bool {
    EU_COUNTRIES.contains(&country)
}

/// One country's VAT rates in basis points: the standard rate and the main
/// reduced rate, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VatRates {
    pub country: &'static str,
    pub standard_bps: i64,
    pub reduced_bps: Option<i64>,
}

/// Standard and main reduced VAT rates of the EU member states, as a
/// starting point for a shop's tax zones. Last reviewed November 2026;
/// rates change, and many countries have more reduced rates, so merchants
/// check them against the European Commission's list.
pub const EU_VAT_RATES: [VatRates; 27] = [
    vat("AT", 2000, Some(1000)),
    vat("BE", 2100, Some(600)),
    vat("BG", 2000, Some(900)),
    vat("CY", 1900, Some(500)),
    vat("CZ", 2100, Some(1200)),
    vat("DE", 1900, Some(700)),
    vat("DK", 2500, None),
    vat("EE", 2400, Some(900)),
    vat("ES", 2100, Some(1000)),
    vat("FI", 2550, Some(1400)),
    vat("FR", 2000, Some(550)),
    vat("GR", 2400, Some(1300)),
    vat("HR", 2500, Some(1300)),
    vat("HU", 2700, Some(500)),
    vat("IE", 2300, Some(1350)),
    vat("IT", 2200, Some(1000)),
    vat("LT", 2100, Some(900)),
    vat("LU", 1700, Some(800)),
    vat("LV", 2100, Some(1200)),
    vat("MT", 1800, Some(700)),
    vat("NL", 2100, Some(900)),
    vat("PL", 2300, Some(800)),
    vat("PT", 2300, Some(600)),
    vat("RO", 2100, Some(1100)),
    vat("SE", 2500, Some(1200)),
    vat("SI", 2200, Some(950)),
    vat("SK", 2300, Some(1900)),
];

const fn vat(country: &'static str, standard_bps: i64, reduced_bps: Option<i64>) -> VatRates {
    VatRates {
        country,
        standard_bps,
        reduced_bps,
    }
}

/// The country a VAT number belongs to, when it is shaped like an EU VAT
/// number: a country prefix (`EL` for Greece) and 2 to 12 letters or
/// digits, spaces, dots and dashes ignored. Only the shape is checked;
/// whether it is registered needs VIES.
pub fn vat_number_country(vat: &str) -> Option<&'static str> {
    let clean: String = vat
        .chars()
        .filter(|c| !matches!(c, ' ' | '.' | '-'))
        .collect::<String>()
        .to_ascii_uppercase();
    if clean.len() < 4 || !clean.is_ascii() {
        return None;
    }
    let (prefix, rest) = clean.split_at(2);
    let country = if prefix == "EL" { "GR" } else { prefix };
    let country = EU_COUNTRIES.iter().find(|c| **c == country)?;
    let ok = (2..=12).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then_some(*country)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert!(is_tax_class("standard"));
        assert!(is_tax_class("reduced_2"));
        assert!(!is_tax_class(""));
        assert!(!is_tax_class("2fast"));
        assert!(!is_tax_class("Reduced"));
        assert!(!is_tax_class(&"a".repeat(33)));
    }

    #[test]
    fn exclusive_tax_is_added_and_rounded() {
        assert_eq!(tax_on(10_000, 1900, false), 1900);
        assert_eq!(tax_on(999, 1900, false), 190); // 189.81
        assert_eq!(tax_on(50, 1000, false), 5);
        assert_eq!(tax_on(5, 1000, false), 1); // 0.5 rounds up
        assert_eq!(tax_on(-999, 1900, false), -190);
        assert_eq!(tax_on(1000, 0, false), 0);
    }

    #[test]
    fn inclusive_tax_is_taken_out_of_the_gross() {
        assert_eq!(tax_on(11_900, 1900, true), 1900);
        assert_eq!(tax_on(1000, 1900, true), 160); // 159.66
        assert_eq!(tax_on(1070, 700, true), 70);
        assert_eq!(tax_on(i64::MAX, 2000, true), i64::MAX / 6); // no overflow
    }

    #[test]
    fn eu_rates_cover_every_member_once() {
        let mut seen: Vec<_> = EU_VAT_RATES.iter().map(|r| r.country).collect();
        seen.sort_unstable();
        assert_eq!(seen, EU_COUNTRIES);
        for r in EU_VAT_RATES {
            assert!(r.standard_bps >= 1500 && r.standard_bps <= MAX_RATE_BPS);
            assert!(r.reduced_bps.is_none_or(|x| x < r.standard_bps));
        }
        assert!(is_eu("DE"));
        assert!(!is_eu("CH"));
    }

    #[test]
    fn vat_numbers() {
        assert_eq!(vat_number_country("DE123456789"), Some("DE"));
        assert_eq!(vat_number_country("de 123.456.789"), Some("DE"));
        assert_eq!(vat_number_country("EL123456789"), Some("GR"));
        assert_eq!(vat_number_country("GR123456789"), Some("GR"));
        assert_eq!(vat_number_country("CHE123456789"), None);
        assert_eq!(vat_number_country("DE1"), None);
        assert_eq!(vat_number_country("DE12345678901234"), None);
        assert_eq!(vat_number_country("DE12_34"), None);
    }
}
