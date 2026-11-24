//! Hook points: the places where code outside SOAT's modules (a plugin,
//! or native code an install adds) may change or veto what SOAT does, and
//! the records it sees there.
//!
//! The WIT package in `soat-plugin-api` (`soat:plugin`) mirrors these
//! records field for field, with kebab-case names; a test there keeps
//! them in step.
//!
//! | Hook | Called | May |
//! | --- | --- | --- |
//! | `discounts` | when a cart's totals are worked out | take amounts off lines or the cart |
//! | `shipping-filter` | when shipping options are listed | hide options or lower their price |
//! | `checkout-validation` | before an order is placed | block it, saying why |
//! | `events` | after any change, at least once | react (no change to the cart) |

use serde::{Deserialize, Serialize};

/// A line of a cart, as hooks see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookLine {
    pub item_id: String,
    pub sku: String,
    pub title: String,
    pub quantity: i64,
    pub unit_price_minor: i64,
    /// After SOAT's own discounts.
    pub line_total_minor: i64,
    /// The product's tags.
    pub tags: Vec<String>,
}

/// A cart, as hooks see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookCart {
    pub currency: String,
    /// Where it ships, once known.
    pub country: Option<String>,
    /// The signed-in customer's tags.
    pub customer_tags: Vec<String>,
    pub lines: Vec<HookLine>,
    /// Sum of the lines' totals.
    pub subtotal_minor: i64,
}

/// An amount a `discounts` hook takes off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adjustment {
    /// The line (index into [`HookCart::lines`]), or none for the cart.
    pub line: Option<u32>,
    /// Taken off, in minor units; never negative.
    pub amount_minor: i64,
    /// Shown to the buyer, e.g. `Loyalty reward`.
    pub label: String,
}

/// A shipping option, as `shipping-filter` hooks see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookShippingOption {
    pub rate_id: String,
    pub name: String,
    pub price_minor: i64,
}

/// What a `checkout-validation` hook decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "reason")]
pub enum Verdict {
    Allow,
    /// Stop the order; the reason is shown to the buyer.
    Block(String),
}

/// An event, as `events` hooks see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEvent {
    /// e.g. `order.created`.
    pub name: String,
    /// The event's payload, as JSON text.
    pub payload: String,
}

/// The hook points, by their WIT world names.
pub const HOOKS: &[&str] = &[
    "discounts",
    "shipping-filter",
    "checkout-validation",
    "events",
];

/// Keeps what a `discounts` hook may do: amounts that are not negative,
/// on lines that exist, never more than a line (or the cart) holds once
/// earlier adjustments are taken off. Others are dropped.
pub fn bounded(cart: &HookCart, adjustments: Vec<Adjustment>) -> Vec<Adjustment> {
    let mut left: Vec<i64> = cart.lines.iter().map(|l| l.line_total_minor).collect();
    let mut cart_left = cart.subtotal_minor;
    let mut out = Vec::new();
    for mut a in adjustments {
        if a.amount_minor <= 0 || a.label.trim().is_empty() {
            continue;
        }
        let room = match a.line {
            Some(i) => match left.get(i as usize) {
                Some(l) => (*l).min(cart_left),
                None => continue,
            },
            None => cart_left,
        };
        a.amount_minor = a.amount_minor.min(room);
        if a.amount_minor <= 0 {
            continue;
        }
        if let Some(i) = a.line {
            left[i as usize] -= a.amount_minor;
        }
        cart_left -= a.amount_minor;
        a.label = a.label.trim().chars().take(100).collect();
        out.push(a);
    }
    out
}

/// Keeps what a `shipping-filter` hook may do: options SOAT offered, in
/// SOAT's order, at a price no higher than SOAT's.
pub fn filtered(
    offered: &[HookShippingOption],
    kept: &[HookShippingOption],
) -> Vec<HookShippingOption> {
    offered
        .iter()
        .filter_map(|o| {
            kept.iter()
                .find(|k| k.rate_id == o.rate_id)
                .map(|k| HookShippingOption {
                    price_minor: k.price_minor.clamp(0, o.price_minor),
                    ..o.clone()
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cart() -> HookCart {
        let line = |total| HookLine {
            item_id: "prod_1".into(),
            sku: "A".into(),
            title: "A".into(),
            quantity: 1,
            unit_price_minor: total,
            line_total_minor: total,
            tags: vec![],
        };
        HookCart {
            currency: "EUR".into(),
            country: Some("DE".into()),
            customer_tags: vec![],
            lines: vec![line(1000), line(500)],
            subtotal_minor: 1500,
        }
    }

    fn adj(line: Option<u32>, amount_minor: i64) -> Adjustment {
        Adjustment {
            line,
            amount_minor,
            label: "Reward".into(),
        }
    }

    #[test]
    fn adjustments_stay_within_the_cart() {
        let out = bounded(
            &cart(),
            vec![
                adj(Some(1), 800),
                adj(Some(7), 100),
                adj(None, -5),
                adj(Some(0), 300),
                adj(None, 5000),
                adj(None, 1),
            ],
        );
        assert_eq!(
            out.iter()
                .map(|a| (a.line, a.amount_minor))
                .collect::<Vec<_>>(),
            vec![(Some(1), 500), (Some(0), 300), (None, 700)]
        );
    }

    #[test]
    fn filters_only_hide_or_lower() {
        let o = |id: &str, p| HookShippingOption {
            rate_id: id.into(),
            name: id.into(),
            price_minor: p,
        };
        let offered = [o("a", 500), o("b", 900), o("c", 0)];
        let kept = [o("c", 300), o("b", 400), o("x", 1)];
        assert_eq!(filtered(&offered, &kept), vec![o("b", 400), o("c", 0)]);
    }

    #[test]
    fn verdicts_read_as_tagged() {
        let v = serde_json::to_value(Verdict::Block("No".into())).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "block", "reason": "No" }));
    }
}
