//! Scripts that SOAT serves itself, so no page asks a third party for
//! them: a visitor's address reaches only the shop until they consent.

/// htmx 1.9.12 (0BSD), byte for byte as published on npm
/// (sha512 `VZAohXyF7xPGS52IM8d1T1283y+X4D+Owf3qY1NZ9RuBypyu9l8cGsxUMAG5fEAb/DhT7rDoJ9Hpu5/HxFD3cw==`
/// of the package).
pub const HTMX_JS: &str = include_str!("../assets/htmx-1.9.12.min.js");

/// The file name it is served under; the version in it lets browsers
/// cache it for good.
pub const HTMX_FILE: &str = "htmx-1.9.12.min.js";

/// A cache header for a file whose name changes with its content.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[cfg(test)]
mod tests {
    #[test]
    fn htmx_is_whole() {
        assert!(super::HTMX_JS.len() > 40_000);
        assert!(super::HTMX_JS.contains("htmx"));
    }
}
