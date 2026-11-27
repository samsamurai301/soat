//! Selling on marketplaces: what an item is offered for there, how many
//! units are offered, and what state an order is in for the marketplace.

/// Units offered of an item whose stock is not tracked.
pub const UNTRACKED_QUANTITY: i64 = 100;

/// The price on a marketplace: `base` (minor units) plus `bps` basis
/// points (rounded half up to a whole minor unit) plus `add`, never below
/// zero.
///
/// ```
/// use soat_core::marketplaces::price;
/// assert_eq!(price(1000, 1500, 0), 1150);
/// assert_eq!(price(999, 1000, 50), 1149); // 999 + 99.9 → 100, + 50
/// assert_eq!(price(1000, -2000, 0), 800);
/// assert_eq!(price(100, 0, -500), 0);
/// ```
pub fn price(base: i64, bps: i64, add: i64) -> i64 {
    let scaled = i128::from(base) * i128::from(bps);
    let markup = if scaled >= 0 {
        (scaled + 5_000) / 10_000
    } else {
        (scaled - 5_000) / 10_000
    };
    let total = i128::from(base) + markup + i128::from(add);
    i64::try_from(total.max(0)).unwrap_or(i64::MAX)
}

/// Units to offer: what is available less `buffer`, never below zero;
/// [`UNTRACKED_QUANTITY`] when stock is not tracked.
///
/// ```
/// use soat_core::marketplaces::quantity;
/// assert_eq!(quantity(true, 5, 2), 3);
/// assert_eq!(quantity(true, 1, 2), 0);
/// assert_eq!(quantity(false, 0, 2), 100);
/// ```
pub fn quantity(tracked: bool, available: i64, buffer: i64) -> i64 {
    if tracked {
        (available - buffer.max(0)).max(0)
    } else {
        UNTRACKED_QUANTITY
    }
}

/// The state a marketplace is told of an order with these SOAT statuses:
/// `canceled`, `shipped` (everything sent), `partially_shipped` or `open`.
///
/// ```
/// use soat_core::marketplaces::state;
/// assert_eq!(state("canceled", "unfulfilled"), "canceled");
/// assert_eq!(state("open", "fulfilled"), "shipped");
/// assert_eq!(state("open", "partially_fulfilled"), "partially_shipped");
/// assert_eq!(state("open", "unfulfilled"), "open");
/// ```
pub fn state(status: &str, fulfillment_status: &str) -> &'static str {
    match (status, fulfillment_status) {
        ("canceled", _) => "canceled",
        (_, "fulfilled") => "shipped",
        (_, "partially_fulfilled") => "partially_shipped",
        _ => "open",
    }
}

/// Marketplaces SOAT knows by name; `other` for any else.
pub const CHANNELS: &[&str] = &[
    "amazon",
    "ebay",
    "otto",
    "kaufland",
    "zalando",
    "bol",
    "cdiscount",
    "allegro",
    "etsy",
    "other",
];
