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
