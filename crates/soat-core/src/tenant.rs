//! Tenants: every shop on a SOAT install is one tenant, and every row it owns
//! carries its id.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A tenant's id: a slug of 2 to 63 lowercase letters, digits and hyphens,
/// starting with a letter and not ending with a hyphen (`acme`, `shop-42`).
/// The same rules as a DNS label, so it can become a subdomain. A few
/// names are [`RESERVED`] for the admin's own pages and common hosts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

/// Why a tenant id was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantIdError(pub String);

impl fmt::Display for TenantIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a valid tenant id (2-63 of a-z, 0-9 and -, starting with a letter, and not a reserved name)",
            self.0
        )
    }
}

impl std::error::Error for TenantIdError {}

/// Ids no shop can take: `/admin/{id}` would be an admin page, or the
/// subdomain is a common host.
pub const RESERVED: &[&str] = &["admin", "api", "login", "logout", "new", "setup", "www"];

impl TenantId {
    pub fn parse(id: &str) -> Result<Self, TenantIdError> {
        let bytes = id.as_bytes();
        let valid = (2..=63).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes[bytes.len() - 1] != b'-'
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            && !RESERVED.contains(&id);
        if valid {
            Ok(Self(id.to_owned()))
        } else {
            Err(TenantIdError(id.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = TenantIdError;
    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::parse(&id)
    }
}

impl From<TenantId> for String {
    fn from(id: TenantId) -> Self {
        id.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dns_label_style_slugs() {
        for ok in ["acme", "shop-42", "a1"] {
            assert_eq!(TenantId::parse(ok).unwrap().as_str(), ok);
        }
    }

    #[test]
    fn rejects_reserved_names() {
        for id in RESERVED {
            assert!(TenantId::parse(id).is_err(), "{id}");
        }
        assert!(TenantId::parse("new-shop").is_ok());
    }

    #[test]
    fn rejects_everything_else() {
        let too_long = "a".repeat(64);
        for bad in [
            "",
            "a",
            "Acme",
            "1shop",
            "shop-",
            "shop_1",
            "shöp",
            too_long.as_str(),
        ] {
            assert!(TenantId::parse(bad).is_err(), "{bad} should be rejected");
        }
    }
}
