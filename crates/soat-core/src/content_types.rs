//! Content types a shop defines for itself (a store location, a size
//! chart, an FAQ entry, a lookbook): a name and typed fields, and entries
//! that fill them in. The field kinds are those of custom fields
//! ([`crate::fields`]).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fields::{check_choices, check_value, is_field_key, FieldKind};

/// Most fields of a content type.
pub const MAX_FIELDS: usize = 50;

/// A field of a content type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentField {
    /// e.g. `opening_hours`: a-z, 0-9 and `_`, starting with a letter.
    pub key: String,
    /// Shown to staff, e.g. `Opening hours`.
    pub name: String,
    /// `text`, `integer`, `decimal`, `boolean`, `date`, `url`, `choice`,
    /// `text_list` or `json`.
    pub kind: String,
    /// The allowed values of a `choice` field.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Every entry must have a value.
    #[serde(default)]
    pub required: bool,
}

/// Checks a content type's fields: 1-50, keys unique and well formed,
/// kinds known, choices only on choice fields. Names are trimmed.
pub fn check_fields(fields: &[ContentField]) -> Result<Vec<ContentField>, String> {
    if fields.is_empty() || fields.len() > MAX_FIELDS {
        return Err(format!("a content type has 1 to {MAX_FIELDS} fields"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        if !is_field_key(&f.key) {
            return Err(format!(
                "`{}` is not a field key: 1-64 of a-z, 0-9 and _, starting with a letter",
                f.key
            ));
        }
        if !seen.insert(f.key.as_str()) {
            return Err(format!("field `{}` appears twice", f.key));
        }
        let name = f.name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(format!(
                "field `{}` needs a name of 1 to 200 characters",
                f.key
            ));
        }
        let kind = FieldKind::parse(&f.kind)
            .ok_or_else(|| format!("field `{}`: unknown kind `{}`", f.key, f.kind))?;
        check_choices(kind, &f.choices).map_err(|e| format!("field `{}`: {e}", f.key))?;
        out.push(ContentField {
            key: f.key.clone(),
            name: name.to_owned(),
            kind: kind.as_str().to_owned(),
            choices: f.choices.clone(),
            required: f.required,
        });
    }
    Ok(out)
}

/// Checks an entry's values against its type's fields and returns them as
/// stored: unknown keys are refused, `null` leaves a field empty, required
/// fields need a value.
pub fn check_entry(
    fields: &[ContentField],
    values: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    if let Some(key) = values.keys().find(|k| !fields.iter().any(|f| &f.key == *k)) {
        return Err(format!("no field `{key}` in this content type"));
    }
    let mut out = Map::new();
    for f in fields {
        match values.get(&f.key) {
            None | Some(Value::Null) => {
                if f.required {
                    return Err(format!("`{}` is required", f.key));
                }
            }
            Some(v) => {
                let kind = FieldKind::parse(&f.kind).unwrap_or(FieldKind::Json);
                let v = check_value(kind, &f.choices, v).map_err(|e| format!("{}: {e}", f.key))?;
                out.insert(f.key.clone(), v);
            }
        }
    }
    Ok(out)
}

/// The first text field's value, to make an entry's handle from.
pub fn title_of(fields: &[ContentField], values: &Map<String, Value>) -> Option<String> {
    fields
        .iter()
        .filter(|f| f.kind == "text")
        .find_map(|f| values.get(&f.key).and_then(Value::as_str))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn field(key: &str, kind: &str) -> ContentField {
        ContentField {
            key: key.into(),
            name: format!(" {key} "),
            kind: kind.into(),
            choices: Vec::new(),
            required: false,
        }
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn fields_are_checked() {
        let ok = check_fields(&[field("name", "text"), field("opens", "date")]).unwrap();
        assert_eq!(ok[0].name, "name");
        assert!(check_fields(&[]).is_err());
        assert!(check_fields(&[field("name", "text"), field("name", "text")]).is_err());
        assert!(check_fields(&[field("Name", "text")]).is_err());
        assert!(check_fields(&[field("name", "colour")]).is_err());
        let mut choice = field("size", "choice");
        assert!(check_fields(std::slice::from_ref(&choice)).is_err());
        choice.choices = vec!["S".into(), "M".into()];
        assert!(check_fields(&[choice]).is_ok());
        let mut blank = field("x", "text");
        blank.name = "  ".into();
        assert!(check_fields(&[blank]).is_err());
        let many: Vec<_> = (0..51).map(|i| field(&format!("f{i}"), "text")).collect();
        assert!(check_fields(&many).is_err());
    }

    #[test]
    fn entries_fit_their_type() {
        let mut name = field("name", "text");
        name.required = true;
        let fields = vec![name, field("seats", "integer"), field("site", "url")];
        let v = check_entry(
            &fields,
            &map(json!({ "name": "Berlin", "seats": 12, "site": null })),
        )
        .unwrap();
        assert_eq!(
            Value::Object(v.clone()),
            json!({ "name": "Berlin", "seats": 12 })
        );
        assert_eq!(title_of(&fields, &v).as_deref(), Some("Berlin"));
        assert!(check_entry(&fields, &map(json!({ "seats": 3 })))
            .unwrap_err()
            .contains("required"));
        assert!(check_entry(&fields, &map(json!({ "name": "x", "seats": "3" }))).is_err());
        assert!(
            check_entry(&fields, &map(json!({ "name": "x", "colour": "red" })))
                .unwrap_err()
                .contains("colour")
        );
    }
}
