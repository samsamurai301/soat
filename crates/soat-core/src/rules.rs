//! Rules that pick products, for smart collections: a field, a comparison
//! and a value, e.g. tag equals `summer`, price below 5000.

use serde::{Deserialize, Serialize};

/// Most rules on one collection.
pub const MAX_RULES: usize = 20;

/// One condition on a product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// `tag`, `title`, `sku`, `tax_class`, `price_minor` or `weight_grams`.
    pub field: String,
    /// For `tag`: `equals` (has it) or `not_equals` (lacks it). For text:
    /// `equals`, `not_equals`, `contains`, `not_contains`, `starts_with`.
    /// For numbers: `equals`, `greater_than`, `less_than`.
    pub op: String,
    /// What to compare with; a whole number for number fields.
    pub value: String,
}

/// What kind of value a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tag,
    Text,
    Number,
}

/// The kind of `field`, if rules may use it.
pub fn kind(field: &str) -> Option<Kind> {
    match field {
        "tag" => Some(Kind::Tag),
        "title" | "sku" | "tax_class" => Some(Kind::Text),
        "price_minor" | "weight_grams" => Some(Kind::Number),
        _ => None,
    }
}

/// Whether `op` applies to fields of `kind`.
pub fn allows(kind: Kind, op: &str) -> bool {
    match kind {
        Kind::Tag => matches!(op, "equals" | "not_equals"),
        Kind::Text => matches!(
            op,
            "equals" | "not_equals" | "contains" | "not_contains" | "starts_with"
        ),
        Kind::Number => matches!(op, "equals" | "greater_than" | "less_than"),
    }
}

/// Checks `rules`, trimming values; says what is wrong with the first bad one.
///
/// # Errors
///
/// Too many rules, an unknown field, a comparison the field does not
/// take, an empty value or a number that is not one.
pub fn check(rules: &mut [Rule]) -> Result<(), String> {
    if rules.len() > MAX_RULES {
        return Err(format!("at most {MAX_RULES} rules"));
    }
    for r in rules.iter_mut() {
        r.value = r.value.trim().to_owned();
        let Some(k) = kind(&r.field) else {
            return Err(format!(
                "{} is not a field rules use: tag, title, sku, tax_class, price_minor or weight_grams",
                r.field
            ));
        };
        if !allows(k, &r.op) {
            return Err(format!("{} does not take {}", r.field, r.op));
        }
        if r.value.is_empty() || r.value.chars().count() > 200 {
            return Err(format!("the value for {} is 1 to 200 characters", r.field));
        }
        if k == Kind::Number && r.value.parse::<i64>().is_err() {
            return Err(format!(
                "{} compares with a whole number, in minor units for prices",
                r.field
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(field: &str, op: &str, value: &str) -> Rule {
        Rule {
            field: field.into(),
            op: op.into(),
            value: value.into(),
        }
    }

    #[test]
    fn rules_are_checked() {
        let mut ok = vec![
            rule("tag", "equals", " summer "),
            rule("price_minor", "less_than", "5000"),
            rule("title", "contains", "tee"),
        ];
        assert!(check(&mut ok).is_ok());
        assert_eq!(ok[0].value, "summer");
        for bad in [
            rule("vendor", "equals", "x"),
            rule("tag", "contains", "x"),
            rule("price_minor", "contains", "5"),
            rule("price_minor", "less_than", "5.00"),
            rule("title", "equals", "  "),
        ] {
            assert!(check(&mut [bad]).is_err());
        }
        let mut many = vec![rule("tag", "equals", "a"); MAX_RULES + 1];
        assert!(check(&mut many).is_err());
    }
}
