//! Custom fields: typed values a shop attaches to its products, customers,
//! orders and more (`care_instructions`, `release_date`).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest text value, in characters.
pub const MAX_TEXT: usize = 10_000;
/// Most entries in a list value.
pub const MAX_LIST: usize = 100;
/// Most choices of a `choice` field.
pub const MAX_CHOICES: usize = 100;

/// What a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// A string up to 10 000 characters.
    Text,
    /// A whole number.
    Integer,
    /// A decimal as a string, e.g. `"12.50"`, so no float rounding.
    Decimal,
    Boolean,
    /// `YYYY-MM-DD`.
    Date,
    /// An `http(s)://` URL.
    Url,
    /// One of the field's choices.
    Choice,
    /// A list of strings.
    TextList,
    /// Any JSON up to 64 KiB.
    Json,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Boolean => "boolean",
            Self::Date => "date",
            Self::Url => "url",
            Self::Choice => "choice",
            Self::TextList => "text_list",
            Self::Json => "json",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }
}

impl fmt::Display for FieldKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A field key: 1-64 of `a-z`, `0-9` and `_`, starting with a letter.
pub fn is_field_key(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Checks the choices of a field: only `choice` fields have them, 1-100
/// distinct non-empty strings.
pub fn check_choices(kind: FieldKind, choices: &[String]) -> Result<(), String> {
    match kind {
        FieldKind::Choice => {
            let mut seen = std::collections::HashSet::new();
            if choices.is_empty() || choices.len() > MAX_CHOICES {
                return Err(format!("a choice field needs 1 to {MAX_CHOICES} choices"));
            }
            for c in choices {
                if c.trim().is_empty() || c.chars().count() > 200 || !seen.insert(c) {
                    return Err(format!("choice `{c}` is empty, too long or repeated"));
                }
            }
            Ok(())
        }
        _ if choices.is_empty() => Ok(()),
        _ => Err("only choice fields have choices".to_owned()),
    }
}

/// Checks `value` against a field and returns it as stored. `Err` says
/// what is wrong, for a 422.
pub fn check_value(kind: FieldKind, choices: &[String], value: &Value) -> Result<Value, String> {
    let wrong = || format!("expected {}", describe(kind));
    match kind {
        FieldKind::Text => {
            let s = value.as_str().ok_or_else(wrong)?;
            if s.chars().count() > MAX_TEXT {
                return Err(format!("text is longer than {MAX_TEXT} characters"));
            }
        }
        FieldKind::Integer => {
            value.as_i64().ok_or_else(wrong)?;
        }
        FieldKind::Decimal => {
            if !value.as_str().is_some_and(is_decimal) {
                return Err(wrong());
            }
        }
        FieldKind::Boolean => {
            value.as_bool().ok_or_else(wrong)?;
        }
        FieldKind::Date => {
            if !value.as_str().is_some_and(is_date) {
                return Err(wrong());
            }
        }
        FieldKind::Url => {
            let s = value.as_str().ok_or_else(wrong)?;
            let rest = s
                .strip_prefix("https://")
                .or_else(|| s.strip_prefix("http://"))
                .ok_or_else(wrong)?;
            if rest.is_empty() || s.len() > 2048 || s.chars().any(char::is_whitespace) {
                return Err(wrong());
            }
        }
        FieldKind::Choice => {
            let s = value.as_str().ok_or_else(wrong)?;
            if !choices.iter().any(|c| c == s) {
                return Err(format!("`{s}` is not one of: {}", choices.join(", ")));
            }
        }
        FieldKind::TextList => {
            let list = value.as_array().ok_or_else(wrong)?;
            if list.len() > MAX_LIST
                || !list
                    .iter()
                    .all(|v| v.as_str().is_some_and(|s| s.chars().count() <= 1000))
            {
                return Err(format!(
                    "expected at most {MAX_LIST} strings of up to 1000 characters"
                ));
            }
        }
        FieldKind::Json => {
            if value.to_string().len() > 64 * 1024 {
                return Err("JSON values are at most 64 KiB".to_owned());
            }
        }
    }
    Ok(value.clone())
}

fn describe(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Text => "a string",
        FieldKind::Integer => "a whole number",
        FieldKind::Decimal => "a decimal string such as \"12.50\"",
        FieldKind::Boolean => "true or false",
        FieldKind::Date => "a date string YYYY-MM-DD",
        FieldKind::Url => "an http(s) URL",
        FieldKind::Choice => "one of the choices",
        FieldKind::TextList => "a list of strings",
        FieldKind::Json => "JSON",
    }
}

fn is_decimal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    !int.is_empty()
        && !frac.is_empty()
        && int.len() <= 30
        && frac.len() <= 10
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_kinds() {
        assert!(is_field_key("care_instructions"));
        assert!(!is_field_key("Care"));
        assert!(!is_field_key("1st"));
        assert!(!is_field_key(""));
        assert_eq!(FieldKind::parse("text_list"), Some(FieldKind::TextList));
        assert_eq!(FieldKind::TextList.as_str(), "text_list");
        assert_eq!(FieldKind::parse("float"), None);
    }

    #[test]
    fn values_match_their_kind() {
        use FieldKind::*;
        let ok = |k, v: Value| check_value(k, &[], &v).is_ok();
        assert!(ok(Text, json!("hi")));
        assert!(!ok(Text, json!(1)));
        assert!(ok(Integer, json!(-3)));
        assert!(!ok(Integer, json!(1.5)));
        assert!(ok(Decimal, json!("12.50")));
        assert!(ok(Decimal, json!("-3")));
        assert!(!ok(Decimal, json!(12.5)));
        assert!(!ok(Decimal, json!("1e3")));
        assert!(ok(Boolean, json!(false)));
        assert!(ok(Date, json!("2028-02-29")));
        assert!(!ok(Date, json!("2027-02-29")));
        assert!(!ok(Date, json!("2027-13-01")));
        assert!(ok(Url, json!("https://example.com/a")));
        assert!(!ok(Url, json!("javascript:alert(1)")));
        assert!(ok(TextList, json!(["a", "b"])));
        assert!(!ok(TextList, json!(["a", 1])));
        assert!(ok(Json, json!({ "any": [1, 2] })));
        let choices = vec!["red".to_owned(), "blue".to_owned()];
        assert!(check_value(Choice, &choices, &json!("red")).is_ok());
        assert!(check_value(Choice, &choices, &json!("green")).is_err());
    }

    #[test]
    fn choices_belong_to_choice_fields() {
        let c = vec!["a".to_owned()];
        assert!(check_choices(FieldKind::Choice, &c).is_ok());
        assert!(check_choices(FieldKind::Choice, &[]).is_err());
        assert!(check_choices(FieldKind::Text, &c).is_err());
        assert!(check_choices(FieldKind::Choice, &["a".to_owned(), "a".to_owned()]).is_err());
    }
}
