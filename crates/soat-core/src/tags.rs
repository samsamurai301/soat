//! Free-form product tags (`summer`, `organic`).

use std::collections::HashSet;

/// Most tags on one item.
pub const MAX_TAGS: usize = 50;
/// Longest tag, in characters.
pub const MAX_TAG_LEN: usize = 100;

/// Trims tags, drops empty ones and repeats (ignoring case, keeping the
/// first spelling). `Err` names the first tag that is too long, or says
/// there are too many.
pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        if tag.chars().count() > MAX_TAG_LEN {
            return Err(format!(
                "tag `{tag}` is longer than {MAX_TAG_LEN} characters"
            ));
        }
        if seen.insert(tag.to_lowercase()) {
            out.push(tag.to_owned());
        }
    }
    if out.len() > MAX_TAGS {
        return Err(format!("at most {MAX_TAGS} tags"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn tags_are_trimmed_and_deduplicated() {
        assert_eq!(
            normalize_tags(&s(&[" Summer", "summer", "", "Organic "])).unwrap(),
            ["Summer", "Organic"]
        );
        assert!(normalize_tags(&s(&[&"x".repeat(101)])).is_err());
        let many: Vec<String> = (0..51).map(|i| i.to_string()).collect();
        assert!(normalize_tags(&many).is_err());
    }
}
