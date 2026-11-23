//! Permissions: what a role, an API key or an AI agent may do.
//!
//! A permission is a `resource:action` name such as `products:write`. Each
//! module declares the permissions it checks; roles grant sets of them, and
//! API keys and Claude's tools are scoped by the very same names.

use serde::Serialize;

/// One permission a module checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Permission {
    /// `resource:action`, lowercase, e.g. `products:write`.
    pub name: &'static str,
    /// One line for people choosing what a role may do.
    pub description: &'static str,
    /// Granted by the built-in `staff` role.
    pub staff_default: bool,
}

impl Permission {
    pub const fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            staff_default: false,
        }
    }

    /// Marks the permission as part of the built-in `staff` role.
    pub const fn for_staff(mut self) -> Self {
        self.staff_default = true;
        self
    }
}

/// Whether `name` is a well-formed `resource:action` permission name:
/// two parts of lowercase letters and underscores, joined by one colon.
pub fn is_permission_name(name: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    matches!(name.split_once(':'), Some((resource, action)) if part(resource) && part(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_resource_colon_action() {
        for ok in ["products:read", "gift_cards:write"] {
            assert!(is_permission_name(ok), "{ok}");
        }
        for bad in [
            "",
            "products",
            "products:",
            ":read",
            "Products:read",
            "a:b:c",
            "a-b:c",
        ] {
            assert!(!is_permission_name(bad), "{bad}");
        }
    }
}
