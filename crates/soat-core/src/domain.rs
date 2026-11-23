//! Domain names a shop is served on (`shop.example.com`).

use std::fmt;

use serde::{Deserialize, Serialize};

/// A lowercase host name with at least two labels, as stored and matched:
/// `Shop.Example.com.` and `shop.example.com:443` both become
/// `shop.example.com`. IP addresses are refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Domain(String);

/// Why a domain was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainError(pub String);

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a valid domain name", self.0)
    }
}

impl std::error::Error for DomainError {}

impl Domain {
    /// Parses a domain as typed by a person.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let name = raw.trim().trim_end_matches('.').to_ascii_lowercase();
        let err = || DomainError(raw.to_owned());
        if name.is_empty() || name.len() > 253 {
            return Err(err());
        }
        let labels: Vec<&str> = name.split('.').collect();
        let label_ok = |l: &&str| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        // The last label is a TLD: never all digits, which also refuses IPv4.
        let tld_ok = labels
            .last()
            .is_some_and(|t| !t.bytes().all(|b| b.is_ascii_digit()));
        if labels.len() < 2 || !labels.iter().all(label_ok) || !tld_ok {
            return Err(err());
        }
        Ok(Self(name))
    }

    /// The domain of an HTTP `Host` header: like [`parse`](Self::parse),
    /// after dropping a `:port`.
    pub fn from_host(host: &str) -> Result<Self, DomainError> {
        let name = match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => host,
        };
        Self::parse(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Domain {
    type Error = DomainError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<Domain> for String {
    fn from(d: Domain) -> Self {
        d.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_trailing_dot_and_port() {
        assert_eq!(
            Domain::parse("Shop.Example.COM.").unwrap().as_str(),
            "shop.example.com"
        );
        assert_eq!(
            Domain::from_host("shop.example.com:8443").unwrap().as_str(),
            "shop.example.com"
        );
        assert_eq!(
            Domain::parse("xn--bcher-kva.de").unwrap().as_str(),
            "xn--bcher-kva.de"
        );
    }

    #[test]
    fn refuses_what_is_not_a_public_name() {
        for bad in [
            "",
            "localhost",
            "example",
            "-a.com",
            "a-.com",
            "a..com",
            "a_b.com",
            "10.0.0.1",
            "[::1]",
            "exa mple.com",
            "http://a.com",
        ] {
            assert!(Domain::parse(bad).is_err(), "{bad}");
        }
        assert!(Domain::from_host("localhost:8080").is_err());
        assert!(Domain::parse(&format!("{}.com", "a".repeat(64))).is_err());
    }
}
