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
    ("setup", include_str!("../admin/setup.html")),
    ("new_shop", include_str!("../admin/new_shop.html")),
    ("welcome", include_str!("../admin/welcome.html")),
    ("launch", include_str!("../admin/launch.html")),
    ("assistant", include_str!("../admin/assistant.html")),
    ("builder", include_str!("../admin/builder.html")),
    ("palette", include_str!("../admin/palette.html")),
    ("collections", include_str!("../admin/collections.html")),
    ("collection", include_str!("../admin/collection.html")),
    ("transfer", include_str!("../admin/transfer.html")),
    ("products", include_str!("../admin/products.html")),
    ("product", include_str!("../admin/product.html")),
    ("orders", include_str!("../admin/orders.html")),
    ("order", include_str!("../admin/order.html")),
    ("customers", include_str!("../admin/customers.html")),
    ("customer", include_str!("../admin/customer.html")),
    ("content_tabs", include_str!("../admin/content_tabs.html")),
    ("pages", include_str!("../admin/pages.html")),
    ("page", include_str!("../admin/page.html")),
    ("menus", include_str!("../admin/menus.html")),
    ("menu", include_str!("../admin/menu.html")),
    ("redirects", include_str!("../admin/redirects.html")),
    ("theme", include_str!("../admin/theme.html")),
    ("theme_file", include_str!("../admin/theme_file.html")),
    ("settings_tabs", include_str!("../admin/settings_tabs.html")),
    (
        "settings_payments",
        include_str!("../admin/settings_payments.html"),
    ),
    (
        "settings_shipping",
        include_str!("../admin/settings_shipping.html"),
    ),
    (
        "settings_taxes",
        include_str!("../admin/settings_taxes.html"),
    ),
    (
        "settings_email",
        include_str!("../admin/settings_email.html"),
    ),
    (
        "settings_invoices",
        include_str!("../admin/settings_invoices.html"),
    ),
    (
        "email_template",
        include_str!("../admin/email_template.html"),
    ),
    (
        "settings_general",
        include_str!("../admin/settings_general.html"),
    ),
    (
        "settings_staff",
        include_str!("../admin/settings_staff.html"),
    ),
    (
        "settings_developers",
        include_str!("../admin/settings_developers.html"),
    ),
    (
        "settings_assistant",
        include_str!("../admin/settings_assistant.html"),
    ),
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
        debug_assert!(content.is_some(), "no admin template {name}");
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

    /// Template `name` alone, for htmx to swap into a page.
    pub fn fragment(&self, name: &str, data: &Value) -> Response {
        let partials = |n: &str| self.0.get(n);
        let html = self
            .0
            .get(name)
            .map(|t| t.render_with(data, true, &partials))
            .unwrap_or_default();
        Html(html).into_response()
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
