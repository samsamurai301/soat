//! Peppol: who sends and receives, and what an access point is told.
//!
//! A Peppol participant is named by an ISO 6523 scheme (the four-digit
//! EAS code, e.g. `0208` for a Belgian enterprise number, `0088` for a
//! GLN, `9930` for a German VAT number) and the id within it:
//! `0208:0123456749`. [`ParticipantId::parse`] takes that form, the full
//! `iso6523-actorid-upis::0208:…` form access points print, and checks the
//! id where its scheme has a check digit or a known shape.
//!
//! ```
//! use soat_core::peppol::ParticipantId;
//!
//! let p = ParticipantId::parse("iso6523-actorid-upis::0208:0123456749").unwrap();
//! assert_eq!((p.scheme(), p.id()), ("0208", "0123456749"));
//! assert_eq!(p.to_string(), "0208:0123456749");
//! assert!(ParticipantId::parse("0208:0123456789").is_err()); // check digits
//! ```

use std::fmt;

/// The identifier scheme of Peppol participant ids.
pub const PARTICIPANT_SCHEME: &str = "iso6523-actorid-upis";

/// The identifier scheme of Peppol document type ids.
pub const DOCUMENT_SCHEME: &str = "busdox-docid-qns";

/// The identifier scheme of Peppol process ids.
pub const PROCESS_SCHEME: &str = "cenbii-procid-ubl";

/// The Peppol BIS Billing 3.0 process.
pub const BILLING_PROCESS: &str = "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0";

/// The document type of a Peppol BIS Billing 3.0 invoice.
pub const INVOICE_DOCUMENT: &str = "urn:oasis:names:specification:ubl:schema:xsd:Invoice-2::Invoice##urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0::2.1";

/// The document type of a Peppol BIS Billing 3.0 credit note.
pub const CREDIT_NOTE_DOCUMENT: &str = "urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2::CreditNote##urn:cen.eu:en16931:2017#compliant#urn:fdc:peppol.eu:2017:poacc:billing:3.0::2.1";

/// The document type id for an invoice or a credit note.
///
/// ```
/// assert!(soat_core::peppol::document_type(true).contains("CreditNote-2"));
/// ```
#[must_use]
pub fn document_type(credit_note: bool) -> &'static str {
    if credit_note {
        CREDIT_NOTE_DOCUMENT
    } else {
        INVOICE_DOCUMENT
    }
}

/// A Peppol participant: an EAS scheme code and the id within it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParticipantId {
    scheme: String,
    id: String,
}

impl ParticipantId {
    /// Reads `scheme:id`, optionally after `iso6523-actorid-upis::`.
    ///
    /// # Errors
    ///
    /// Says what is wrong, for a person to read: no scheme, a scheme that
    /// is not four digits, an id with spaces or too long, or an id that
    /// fails its scheme's check.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let s = match s.split_once("::") {
            Some((prefix, rest)) if prefix.eq_ignore_ascii_case(PARTICIPANT_SCHEME) => rest,
            Some(_) => return Err(format!("a Peppol id starts with {PARTICIPANT_SCHEME}:: or the scheme, as in 0208:0123456749")),
            None => s,
        };
        let Some((scheme, id)) = s.split_once(':') else {
            return Err("a Peppol id is the scheme and the id, as in 0208:0123456749".into());
        };
        if scheme.len() != 4 || !scheme.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "{scheme} is not a Peppol scheme: four digits, as in 0208 or 9930"
            ));
        }
        // Peppol policy: printable ASCII without spaces, at most 50 in all.
        if id.is_empty()
            || scheme.len() + 1 + id.len() > 50
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(
                "the id after the scheme is 1 to 45 letters, digits or signs, without spaces"
                    .into(),
            );
        }
        check(scheme, id)?;
        Ok(Self {
            scheme: scheme.to_owned(),
            id: id.to_owned(),
        })
    }

    /// The EAS code, e.g. `0208`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The id within the scheme.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The form access points and SMP lookups use:
    /// `iso6523-actorid-upis::0208:0123456749`.
    ///
    /// ```
    /// let p = soat_core::peppol::ParticipantId::parse("9930:DE123456789").unwrap();
    /// assert_eq!(p.full(), "iso6523-actorid-upis::9930:DE123456789");
    /// ```
    #[must_use]
    pub fn full(&self) -> String {
        format!("{PARTICIPANT_SCHEME}::{self}")
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.scheme, self.id)
    }
}

fn digits(id: &str, len: usize) -> Option<Vec<u32>> {
    (id.len() == len)
        .then(|| {
            id.chars()
                .map(|c| c.to_digit(10))
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
}

/// The checks of the schemes shops meet most.
fn check(scheme: &str, id: &str) -> Result<(), String> {
    let ok = match scheme {
        // Belgian enterprise number: 10 digits, the last two 97 - (the first eight mod 97).
        "0208" => digits(id, 10).is_some_and(|d| {
            let body = d[..8].iter().fold(0u64, |n, &x| n * 10 + u64::from(x));
            let sum = u64::from(d[8] * 10 + d[9]);
            d[0] <= 1 && 97 - body % 97 == sum
        }),
        // GS1 GLN: 13 digits with the GS1 check digit.
        "0088" => digits(id, 13).is_some_and(|d| gs1(&d)),
        // DUNS: 9 digits.
        "0060" => digits(id, 9).is_some(),
        // Norwegian organisation number: 9 digits, mod 11.
        "0192" => digits(id, 9).is_some_and(|d| {
            let w = [3, 2, 7, 6, 5, 4, 3, 2];
            let r = d[..8].iter().zip(w).map(|(x, w)| x * w).sum::<u32>() % 11;
            let c = if r == 0 { 0 } else { 11 - r };
            c < 10 && c == d[8]
        }),
        // Swedish organisation number: 10 digits.
        "0007" => digits(id, 10).is_some(),
        // VAT numbers: the country's prefix, then letters and digits.
        "9906" | "9914" | "9920" | "9925" | "9930" | "9944" | "9957" => {
            let country = match scheme {
                "9906" => "IT",
                "9914" => "AT",
                "9920" => "ES",
                "9925" => "BE",
                "9930" => "DE",
                "9944" => "NL",
                _ => "FR",
            };
            let vat = id;
            return if vat.len() > 4
                && vat[..2].eq_ignore_ascii_case(country)
                && vat[2..].bytes().all(|b| b.is_ascii_alphanumeric())
            {
                Ok(())
            } else {
                Err(format!(
                    "{scheme} is the {country} VAT number scheme: the id starts with {country}"
                ))
            };
        }
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{id} is not a valid id in scheme {scheme}"))
    }
}

fn gs1(d: &[u32]) -> bool {
    let (body, last) = d.split_at(d.len() - 1);
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, x)| if i % 2 == 0 { x * 3 } else { *x })
        .sum();
    (10 - sum % 10) % 10 == last[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_forms() {
        let a = ParticipantId::parse("0208:0123456749").unwrap();
        let b = ParticipantId::parse(" ISO6523-ACTORID-UPIS::0208:0123456749 ").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.full(), "iso6523-actorid-upis::0208:0123456749");
    }

    #[test]
    fn checks_the_schemes_shops_meet_most() {
        for good in [
            "0208:0123456749",
            "0088:4000001000005",
            "0192:974760673",
            "9930:DE123456789",
            "9925:BE0123456749",
            "0007:5560360793",
            "0060:123456789",
            "0151:47555222000",
            "0204:991-1234512345-06",
        ] {
            assert!(ParticipantId::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "0208:0123456789",
            "0208:2123456789",
            "0088:4000001000006",
            "0192:974760674",
            "9930:FR123456789",
            "0007:556036079",
            "208:0123456749",
            "abcd:1",
            "0208:",
            "0208:01 23",
            "0123456749",
            "peppol::0208:0123456749",
            &format!("0204:{}", "1".repeat(46)),
        ] {
            assert!(ParticipantId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_the_document_type() {
        assert!(document_type(false)
            .starts_with("urn:oasis:names:specification:ubl:schema:xsd:Invoice-2::Invoice##"));
        assert!(CREDIT_NOTE_DOCUMENT.ends_with("billing:3.0::2.1"));
    }
}
