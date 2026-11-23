//! The admin's templates, rendered inside its layout.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use soat_core::template::Template;
use zapline::prelude::*;

/// The admin's stylesheet.
pub(crate) const CSS: &str = include_str!("../admin/admin.css");

const FILES: &[(&str, &str)] = &[
    ("layout", include_str!("../admin/layout.html")),
    ("login", include_str!("../admin/login.html")),
    ("shops", include_str!("../admin/shops.html")),
    ("home", include_str!("../admin/home.html")),
    ("error", include_str!("../admin/error.html")),
    ("products", include_str!("../admin/products.html")),
    ("product", include_str!("../admin/product.html")),
    ("orders", include_str!("../admin/orders.html")),
    ("order", include_str!("../admin/order.html")),
    ("customers", include_str!("../admin/customers.html")),
    ("customer", include_str!("../admin/customer.html")),
];

pub(crate) struct Pages(BTreeMap<&'static str, Template>);

impl Pages {
    pub fn load() -> Self {
        Self(
            FILES
                .iter()
                .map(|(name, source)| {
                    let t = Template::parse(source)
                        .unwrap_or_else(|e| panic!("admin template {name}: {}", e.0));
                    (*name, t)
                })
                .collect(),
        )
    }

    /// Page `name` inside the layout, as an HTML response.
    pub fn render(&self, name: &str, data: &Value, status: StatusCode) -> Response {
        let content = self.0.get(name);
        let partials = |n: &str| -> Option<&Template> {
            if n == "content" {
                content
            } else {
                self.0.get(n)
            }
        };
        let html = self
            .0
            .get("layout")
            .map(|l| l.render_with(data, true, &partials))
            .unwrap_or_default();
        (status, Html(html)).into_response()
    }

    /// A page that says what went wrong.
    pub fn error(&self, status: StatusCode, message: &str) -> Response {
        self.render(
            "error",
            &json!({ "page_title": status.canonical_reason().unwrap_or("Error"), "message": message }),
            status,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_parses() {
        let p = Pages::load();
        assert_eq!(p.0.len(), FILES.len());
    }
}
