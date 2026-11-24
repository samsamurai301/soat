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

use std::collections::HashMap;
use std::sync::Arc;

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

/// A setting a hook asks each shop for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSetting {
    pub key: String,
    pub label: String,
    /// Kept sealed, never shown again (API keys).
    #[serde(default)]
    pub secret: bool,
}

/// Code run at the hook points: native Rust an install adds, or (with
/// the plugin host) a plugin. Every method has a default that changes
/// nothing; `shop` is the shop's id.
pub trait Hook: Send + Sync {
    /// Names it in logs and errors, and to shops.
    fn name(&self) -> &str;

    /// Whether it runs only for shops that turn it on (plugins do);
    /// otherwise it runs for every shop.
    fn per_shop(&self) -> bool {
        false
    }

    /// What it asks a shop to fill in.
    fn settings(&self) -> Vec<HookSetting> {
        Vec::new()
    }

    /// This hook with a shop's settings over the install's, or none when
    /// it takes no settings.
    fn with_settings(&self, settings: &HashMap<String, String>) -> Option<Arc<dyn Hook>> {
        let _ = settings;
        None
    }

    /// Amounts to take off the cart.
    fn discount(&self, shop: &str, cart: &HookCart) -> Vec<Adjustment> {
        let _ = (shop, cart);
        Vec::new()
    }

    /// The shipping options to offer, from those given.
    fn filter_shipping(
        &self,
        shop: &str,
        cart: &HookCart,
        options: &[HookShippingOption],
    ) -> Vec<HookShippingOption> {
        let _ = (shop, cart);
        options.to_vec()
    }

    /// Whether the order may be placed.
    fn validate(&self, shop: &str, cart: &HookCart) -> Verdict {
        let _ = (shop, cart);
        Verdict::Allow
    }
}

/// The hooks of this install, run in the order added; an app state.
#[derive(Clone, Default)]
pub struct Hooks(Arc<Vec<Arc<dyn Hook>>>);

impl Hooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `hook` after those already there.
    pub fn with(self, hook: impl Hook + 'static) -> Self {
        let mut list: Vec<Arc<dyn Hook>> = self.0.iter().cloned().collect();
        list.push(Arc::new(hook));
        Self(Arc::new(list))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The hooks, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Hook>> {
        self.0.iter()
    }

    /// Whether any hook runs only for shops that turn it on.
    pub fn has_per_shop(&self) -> bool {
        self.0.iter().any(|h| h.per_shop())
    }

    /// The hooks one shop runs: every hook that is not [`Hook::per_shop`],
    /// and those it turned on (named in `on`), with its settings.
    pub fn for_shop(&self, on: &HashMap<String, HashMap<String, String>>) -> Hooks {
        let list = self
            .0
            .iter()
            .filter_map(|h| {
                if !h.per_shop() {
                    return Some(h.clone());
                }
                let settings = on.get(h.name())?;
                Some(h.with_settings(settings).unwrap_or_else(|| h.clone()))
            })
            .collect();
        Hooks(Arc::new(list))
    }

    /// Every hook's adjustments, each kept [`bounded`] by what the ones
    /// before it took.
    pub fn discounts(&self, shop: &str, cart: &HookCart) -> Vec<Adjustment> {
        let mut out: Vec<Adjustment> = Vec::new();
        for hook in self.0.iter() {
            let mut seen = cart.clone();
            for a in &out {
                if let Some(l) = a.line.and_then(|i| seen.lines.get_mut(i as usize)) {
                    l.line_total_minor -= a.amount_minor;
                }
                seen.subtotal_minor -= a.amount_minor;
            }
            out.extend(bounded(&seen, hook.discount(shop, &seen)));
        }
        out
    }

    /// The options every hook keeps, each [`filtered`].
    pub fn filter_shipping(
        &self,
        shop: &str,
        cart: &HookCart,
        offered: &[HookShippingOption],
    ) -> Vec<HookShippingOption> {
        let mut options = offered.to_vec();
        for hook in self.0.iter() {
            let kept = hook.filter_shipping(shop, cart, &options);
            options = filtered(&options, &kept);
        }
        options
    }

    /// The first hook's block, with its reason, or allow.
    pub fn validate(&self, shop: &str, cart: &HookCart) -> Verdict {
        for hook in self.0.iter() {
            if let Verdict::Block(reason) = hook.validate(shop, cart) {
                let reason = reason.trim();
                let reason = if reason.is_empty() {
                    format!("{} does not allow this order", hook.name())
                } else {
                    reason.chars().take(300).collect()
                };
                return Verdict::Block(reason);
            }
        }
        Verdict::Allow
    }
}

/// Splits `amount` over `weights` in proportion, in whole units, the
/// largest remainders getting the leftovers; never more than a weight.
pub fn spread(amount: i64, weights: &[i64]) -> Vec<i64> {
    let total: i64 = weights.iter().map(|w| (*w).max(0)).sum();
    if amount <= 0 || total <= 0 {
        return vec![0; weights.len()];
    }
    let amount = amount.min(total);
    let mut shares: Vec<(usize, i64, i128)> = weights
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let exact = i128::from(amount) * i128::from((*w).max(0));
            let share = exact / i128::from(total);
            (i, share as i64, exact % i128::from(total))
        })
        .collect();
    let mut left = amount - shares.iter().map(|s| s.1).sum::<i64>();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|a, b| shares[*b].2.cmp(&shares[*a].2).then(a.cmp(b)));
    for i in order {
        if left == 0 {
            break;
        }
        if shares[i].1 < weights[i] {
            shares[i].1 += 1;
            left -= 1;
        }
    }
    shares.into_iter().map(|s| s.1).collect()
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
    fn amounts_spread_in_proportion() {
        assert_eq!(spread(100, &[1000, 500]), vec![67, 33]);
        assert_eq!(spread(10, &[1, 1, 1]), vec![1, 1, 1]);
        assert_eq!(spread(2, &[1, 1, 1]), vec![1, 1, 0]);
        assert_eq!(spread(5, &[0, 0]), vec![0, 0]);
        assert_eq!(spread(-1, &[3]), vec![0]);
    }

    struct Half;
    impl Hook for Half {
        fn name(&self) -> &str {
            "half"
        }
        fn discount(&self, _: &str, cart: &HookCart) -> Vec<Adjustment> {
            vec![adj(None, cart.subtotal_minor / 2)]
        }
        fn validate(&self, _: &str, cart: &HookCart) -> Verdict {
            if cart.lines.len() > 1 {
                Verdict::Block(" ".into())
            } else {
                Verdict::Allow
            }
        }
    }

    #[test]
    fn hooks_run_in_turn() {
        let hooks = Hooks::new().with(Half).with(Half);
        let out = hooks.discounts("acme", &cart());
        assert_eq!(
            out.iter().map(|a| a.amount_minor).collect::<Vec<_>>(),
            vec![750, 375]
        );
        assert_eq!(
            hooks.validate("acme", &cart()),
            Verdict::Block("half does not allow this order".into())
        );
        assert!(Hooks::new().is_empty());
    }

    #[test]
    fn verdicts_read_as_tagged() {
        let v = serde_json::to_value(Verdict::Block("No".into())).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "block", "reason": "No" }));
    }

    /// Takes its `amount` setting off the first line.
    struct Fixed(&'static str, bool, i64);
    impl Hook for Fixed {
        fn name(&self) -> &str {
            self.0
        }
        fn per_shop(&self) -> bool {
            self.1
        }
        fn with_settings(&self, s: &HashMap<String, String>) -> Option<Arc<dyn Hook>> {
            let amount = s.get("amount")?.parse().ok()?;
            Some(Arc::new(Fixed(self.0, self.1, amount)))
        }
        fn discount(&self, _: &str, _: &HookCart) -> Vec<Adjustment> {
            vec![adj(Some(0), self.2)]
        }
    }

    #[test]
    fn a_shop_runs_its_own_hooks_with_its_settings() {
        let hooks = Hooks::new()
            .with(Fixed("native", false, 1))
            .with(Fixed("plugin", true, 2))
            .with(Fixed("other", true, 3));
        assert!(hooks.has_per_shop());
        let amounts = |h: Hooks| -> Vec<i64> {
            h.discounts("s", &cart())
                .iter()
                .map(|a| a.amount_minor)
                .collect()
        };
        assert_eq!(amounts(hooks.for_shop(&HashMap::new())), vec![1]);
        let on = HashMap::from([(
            "plugin".to_string(),
            HashMap::from([("amount".to_string(), "20".to_string())]),
        )]);
        assert_eq!(amounts(hooks.for_shop(&on)), vec![1, 20]);
        let on = HashMap::from([("other".to_string(), HashMap::new())]);
        assert_eq!(amounts(hooks.for_shop(&on)), vec![1, 3]);
    }
}
