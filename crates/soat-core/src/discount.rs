//! Discounts: what a code takes off, and sharing it out across lines.

/// What a discount does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscountKind {
    /// A share of the goods, in basis points (1000 = 10%).
    Percentage(i64),
    /// An amount off the goods, in minor units, never more than they cost.
    FixedAmount(i64),
    /// Shipping costs nothing.
    FreeShipping,
}

/// `(off the goods, off shipping)` for an order of `subtotal` goods and
/// `shipping`.
pub fn amounts(kind: DiscountKind, subtotal: i64, shipping: i64) -> (i64, i64) {
    match kind {
        DiscountKind::Percentage(bps) => {
            let off = i128::from(subtotal) * i128::from(bps.clamp(0, 10_000)) / 10_000;
            (off as i64, 0)
        }
        DiscountKind::FixedAmount(amount) => (amount.clamp(0, subtotal.max(0)), 0),
        DiscountKind::FreeShipping => (0, shipping.max(0)),
    }
}

/// Shares `total` out over `amounts` in proportion, in whole minor units;
/// what rounding leaves goes to the largest amounts first. The shares add
/// up to `total` (capped at the sum of `amounts`) and none exceeds its
/// amount.
pub fn allocate(total: i64, amounts: &[i64]) -> Vec<i64> {
    let sum: i64 = amounts.iter().map(|a| (*a).max(0)).sum();
    let total = total.clamp(0, sum);
    if sum == 0 {
        return vec![0; amounts.len()];
    }
    let mut shares: Vec<i64> = amounts
        .iter()
        .map(|a| (i128::from((*a).max(0)) * i128::from(total) / i128::from(sum)) as i64)
        .collect();
    let mut left = total - shares.iter().sum::<i64>();
    let mut order: Vec<usize> = (0..amounts.len()).collect();
    order.sort_by_key(|i| std::cmp::Reverse(amounts[*i]));
    for i in order.into_iter().cycle() {
        if left == 0 {
            break;
        }
        if shares[i] < amounts[i] {
            shares[i] += 1;
            left -= 1;
        }
    }
    shares
}

/// Buy X get Y: for every `buy + get` units across `lines` (`(unit
/// price, quantity)`), the cheapest `get` units of the lot are `bps` off
/// (10000 = free). Answers what comes off each line, in minor units.
pub fn buy_x_get_y(lines: &[(i64, i64)], buy: i64, get: i64, bps: i64) -> Vec<i64> {
    let mut off = vec![0; lines.len()];
    if buy < 1 || get < 1 {
        return off;
    }
    let units: i64 = lines.iter().map(|(_, q)| (*q).max(0)).sum();
    let mut cheap = units / (buy + get) * get;
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|i| lines[*i].0);
    let bps = i128::from(bps.clamp(0, 10_000));
    for i in order {
        if cheap == 0 {
            break;
        }
        let (price, qty) = lines[i];
        let n = qty.max(0).min(cheap);
        cheap -= n;
        off[i] = (i128::from(price.max(0)) * i128::from(n) * bps / 10_000) as i64;
    }
    off
}

/// A discount code as people type it: trimmed and uppercased; 1 to 50 of
/// `A-Z`, `0-9`, `-` and `_`.
pub fn normalize_code(raw: &str) -> Option<String> {
    let code = raw.trim().to_ascii_uppercase();
    let ok = (1..=50).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    ok.then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_off() {
        assert_eq!(amounts(DiscountKind::Percentage(1000), 3450, 495), (345, 0));
        assert_eq!(amounts(DiscountKind::Percentage(20_000), 100, 0), (100, 0));
        assert_eq!(amounts(DiscountKind::FixedAmount(500), 300, 0), (300, 0));
        assert_eq!(amounts(DiscountKind::FixedAmount(500), 3000, 0), (500, 0));
        assert_eq!(amounts(DiscountKind::FreeShipping, 3000, 495), (0, 495));
    }

    #[test]
    fn allocation() {
        assert_eq!(allocate(345, &[2380, 1070]), vec![238, 107]);
        assert_eq!(allocate(10, &[1, 1, 1]), vec![1, 1, 1]);
        assert_eq!(allocate(2, &[1, 1, 1]), vec![1, 1, 0]);
        assert_eq!(allocate(100, &[33, 33, 34]), vec![33, 33, 34]);
        assert_eq!(allocate(5, &[0, 0]), vec![0, 0]);
        let s = allocate(1000, &[333, 333, 334]);
        assert_eq!(s.iter().sum::<i64>(), 1000);
        let s = allocate(7, &[10, 20, 30]);
        assert_eq!(s.iter().sum::<i64>(), 7);
        assert!(s.iter().zip([10, 20, 30]).all(|(a, b)| *a <= b));
    }

    #[test]
    fn buy_two_get_one() {
        // Five units: one lot of three, so the cheapest unit is free.
        assert_eq!(
            buy_x_get_y(&[(1000, 3), (500, 2)], 2, 1, 10_000),
            vec![0, 500]
        );
        // Six units: two lots, the two cheapest are half price.
        assert_eq!(
            buy_x_get_y(&[(1000, 4), (600, 1), (400, 1)], 2, 1, 5000),
            vec![0, 300, 200]
        );
        assert_eq!(buy_x_get_y(&[(1000, 2)], 2, 1, 10_000), vec![0]);
        assert_eq!(buy_x_get_y(&[(1000, 2)], 1, 1, 10_000), vec![1000]);
        assert_eq!(buy_x_get_y(&[(1000, 9)], 0, 1, 10_000), vec![0]);
    }

    #[test]
    fn codes() {
        assert_eq!(normalize_code(" summer-10 "), Some("SUMMER-10".into()));
        assert_eq!(normalize_code("no spaces"), None);
        assert_eq!(normalize_code(""), None);
    }
}
