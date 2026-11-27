//! Taking a person out of recorded data, e.g. the event log, when a shop
//! erases a customer (GDPR art. 17).

use serde_json::Value;

/// Fields that name, reach or locate a person.
pub const PERSONAL_KEYS: &[&str] = &[
    "email",
    "first_name",
    "last_name",
    "phone",
    "company",
    "line1",
    "line2",
    "postal_code",
    "city",
    "region",
    "author_name",
    "recipient",
    "recipient_email",
    "vat_number",
    "shipping_address",
    "billing_address",
];

/// What erased values become.
pub const ERASED: &str = "[erased]";

/// Replaces in `value`, at any depth, the values of [`PERSONAL_KEYS`] and
/// any string that contains `email` (ignoring case) with [`ERASED`].
/// Whether anything changed.
///
/// ```
/// use serde_json::json;
/// use soat_core::privacy::redact;
/// let mut v = json!({ "id": "cus_1", "email": "ann@example.com", "tags": ["vip"],
///                     "note": "Call ANN@example.com",
///                     "shipping_address": { "line1": "Main St 1" } });
/// assert!(redact(&mut v, "ann@example.com"));
/// assert_eq!(v, json!({ "id": "cus_1", "email": "[erased]", "tags": ["vip"],
///                       "note": "[erased]", "shipping_address": "[erased]" }));
/// assert!(!redact(&mut v, "ann@example.com"));
/// ```
pub fn redact(value: &mut Value, email: &str) -> bool {
    let email = email.trim().to_lowercase();
    walk(value, &email)
}

fn walk(value: &mut Value, email: &str) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (k, v) in map.iter_mut() {
                let personal = PERSONAL_KEYS.contains(&k.as_str())
                    && !matches!(v, Value::Null)
                    && *v != Value::String(String::new())
                    && *v != Value::String(ERASED.into());
                if personal {
                    *v = Value::String(ERASED.into());
                    changed = true;
                } else {
                    changed |= walk(v, email);
                }
            }
            changed
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |changed, v| walk(v, email) | changed),
        Value::String(s) if !email.is_empty() && s.to_lowercase().contains(email) => {
            *s = ERASED.into();
            true
        }
        _ => false,
    }
}

/// What a visitor agreed to, beyond what the shop needs to work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Consent {
    /// The version of the shop's banner it was given for; a new version
    /// asks again.
    pub version: i64,
    /// Measuring visits with a third party (e.g. Google Analytics).
    pub analytics: bool,
    /// Ads and their measuring (e.g. the Meta Pixel).
    pub marketing: bool,
    /// Features that apps load from their own servers.
    pub apps: bool,
}

impl Consent {
    /// Agreeing to everything, or to nothing.
    pub fn all(version: i64, yes: bool) -> Self {
        Self {
            version,
            analytics: yes,
            marketing: yes,
            apps: yes,
        }
    }

    /// As a cookie value, e.g. `3.am` (version 3, analytics and marketing).
    ///
    /// ```
    /// use soat_core::privacy::Consent;
    /// let c = Consent { version: 3, analytics: true, marketing: true, apps: false };
    /// assert_eq!(c.cookie(), "3.am");
    /// assert_eq!(Consent::parse("3.am"), Some(c));
    /// assert_eq!(Consent::parse("2."), Some(Consent::all(2, false)));
    /// assert_eq!(Consent::parse("x.am"), None);
    /// assert_eq!(Consent::parse("3.zz"), None);
    /// ```
    pub fn cookie(&self) -> String {
        let mut flags = String::new();
        if self.analytics {
            flags.push('a');
        }
        if self.marketing {
            flags.push('m');
        }
        if self.apps {
            flags.push('x');
        }
        format!("{}.{flags}", self.version)
    }

    /// A cookie value from [`Consent::cookie`]; `None` when it is not one.
    pub fn parse(value: &str) -> Option<Self> {
        let (version, flags) = value.split_once('.')?;
        let version: i64 = version.parse().ok().filter(|v| *v > 0)?;
        if flags.len() > 3 || !flags.chars().all(|c| matches!(c, 'a' | 'm' | 'x')) {
            return None;
        }
        Some(Self {
            version,
            analytics: flags.contains('a'),
            marketing: flags.contains('m'),
            apps: flags.contains('x'),
        })
    }
}
