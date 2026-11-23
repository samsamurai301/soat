//! Product options (Size, Color...) and the variants they define.
//!
//! A product has at most [`MAX_OPTIONS`] options, each with its own ordered
//! values. A variant picks exactly one value per option, in option order,
//! and no two variants of a product pick the same values.

use std::collections::HashSet;
use std::fmt;

/// Most options a product can have.
pub const MAX_OPTIONS: usize = 3;
/// Most values one option can have.
pub const MAX_VALUES: usize = 100;
/// Most variants one product can have.
pub const MAX_VARIANTS: usize = 100;

/// Why options or a variant's values were rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionsError {
    TooManyOptions,
    /// An empty or overlong name, or one used twice (ignoring case).
    BadName(String),
    /// An option with no values or more than [`MAX_VALUES`].
    ValueCount(String),
    /// An empty or overlong value, or one listed twice in its option.
    BadValue {
        option: String,
        value: String,
    },
    /// A variant names a different number of values than there are options.
    WrongArity {
        expected: usize,
        got: usize,
    },
    /// A variant uses a value its option does not have.
    UnknownValue {
        option: String,
        value: String,
    },
}

impl fmt::Display for OptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyOptions => write!(f, "a product has at most {MAX_OPTIONS} options"),
            Self::BadName(n) => write!(f, "option name `{n}` is empty, too long or repeated"),
            Self::ValueCount(n) => {
                write!(f, "option `{n}` needs between 1 and {MAX_VALUES} values")
            }
            Self::BadValue { option, value } => write!(
                f,
                "value `{value}` of option `{option}` is empty, too long or repeated"
            ),
            Self::WrongArity { expected, got } => write!(
                f,
                "a variant needs one value per option: expected {expected}, got {got}"
            ),
            Self::UnknownValue { option, value } => {
                write!(f, "option `{option}` has no value `{value}`")
            }
        }
    }
}

impl std::error::Error for OptionsError {}

/// Checks a product's options: `(name, values)` in order.
pub fn check_options<'a, V>(options: &'a [(String, V)]) -> Result<(), OptionsError>
where
    V: AsRef<[String]> + 'a,
{
    if options.len() > MAX_OPTIONS {
        return Err(OptionsError::TooManyOptions);
    }
    let mut names = HashSet::new();
    for (name, values) in options {
        let values = values.as_ref();
        if !text_ok(name, 50) || !names.insert(name.trim().to_lowercase()) {
            return Err(OptionsError::BadName(name.clone()));
        }
        if values.is_empty() || values.len() > MAX_VALUES {
            return Err(OptionsError::ValueCount(name.clone()));
        }
        let mut seen = HashSet::new();
        for value in values {
            if !text_ok(value, 100) || !seen.insert(value.trim().to_lowercase()) {
                return Err(OptionsError::BadValue {
                    option: name.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Checks that a variant's `values` pick one value of each option, in order.
pub fn check_variant<V>(options: &[(String, V)], values: &[String]) -> Result<(), OptionsError>
where
    V: AsRef<[String]>,
{
    if values.len() != options.len() {
        return Err(OptionsError::WrongArity {
            expected: options.len(),
            got: values.len(),
        });
    }
    for ((name, allowed), value) in options.iter().zip(values) {
        if !allowed.as_ref().contains(value) {
            return Err(OptionsError::UnknownValue {
                option: name.clone(),
                value: value.clone(),
            });
        }
    }
    Ok(())
}

/// A variant's title from its values: `M / Red`, or `Default` without options.
pub fn variant_title(values: &[String]) -> String {
    if values.is_empty() {
        "Default".to_owned()
    } else {
        values.join(" / ")
    }
}

fn text_ok(s: &str, max: usize) -> bool {
    let t = s.trim();
    !t.is_empty() && t == s && s.chars().count() <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(name: &str, values: &[&str]) -> (String, Vec<String>) {
        (
            name.to_owned(),
            values.iter().map(|v| (*v).to_owned()).collect(),
        )
    }

    fn vals(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn options_are_checked() {
        let ok = [opt("Size", &["S", "M"]), opt("Color", &["Red"])];
        assert_eq!(check_options(&ok), Ok(()));
        let four: Vec<_> = (0..4).map(|i| opt(&format!("O{i}"), &["a"])).collect();
        assert_eq!(check_options(&four), Err(OptionsError::TooManyOptions));
        assert!(matches!(
            check_options(&[opt("Size", &["S"]), opt("size", &["M"])]),
            Err(OptionsError::BadName(_))
        ));
        assert!(matches!(
            check_options(&[opt("Size", &[])]),
            Err(OptionsError::ValueCount(_))
        ));
        assert!(matches!(
            check_options(&[opt("Size", &["S", "s"])]),
            Err(OptionsError::BadValue { .. })
        ));
        assert!(matches!(
            check_options(&[opt(" Size", &["S"])]),
            Err(OptionsError::BadName(_))
        ));
    }

    #[test]
    fn variants_pick_one_value_per_option() {
        let o = [opt("Size", &["S", "M"]), opt("Color", &["Red"])];
        assert_eq!(check_variant(&o, &vals(&["M", "Red"])), Ok(()));
        assert_eq!(
            check_variant(&o, &vals(&["M"])),
            Err(OptionsError::WrongArity {
                expected: 2,
                got: 1
            })
        );
        assert!(matches!(
            check_variant(&o, &vals(&["Red", "M"])),
            Err(OptionsError::UnknownValue { .. })
        ));
        let none: [(String, Vec<String>); 0] = [];
        assert_eq!(check_variant(&none, &[]), Ok(()));
    }

    #[test]
    fn titles() {
        assert_eq!(variant_title(&vals(&["M", "Red"])), "M / Red");
        assert_eq!(variant_title(&[]), "Default");
    }
}
