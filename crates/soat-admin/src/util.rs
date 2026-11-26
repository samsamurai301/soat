//! Helpers the admin's pages share.

use serde_json::Value;
use soat_core::Currency;

/// An API error in a person's words.
pub(crate) fn message(body: &Value) -> String {
    if let Some(d) = body["details"].as_array().and_then(|d| d.first()) {
        let field = d["field"].as_str().unwrap_or_default();
        let msg = d["message"].as_str().unwrap_or("is not valid");
        return format!("{field}: {msg}");
    }
    body["error"]
        .as_str()
        .unwrap_or("Something went wrong.")
        .to_owned()
}

pub(crate) fn money(minor: i64, currency: &str) -> String {
    Currency::parse(currency).map_or_else(|_| format!("{minor} {currency}"), |c| c.format(minor))
}

/// `s` safe inside one path segment.
pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
