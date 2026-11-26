//! Invoice arithmetic: net, tax and gross per line, totals per tax rate
//! (as EU invoices must show them), and splitting a refund over the rates
//! of the invoice it credits.

use serde::{Deserialize, Serialize};

use crate::discount::allocate;
use crate::tax::tax_on;

/// An amount split into net, tax and gross (`net + tax = gross`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Amounts {
    pub net_minor: i64,
    pub tax_minor: i64,
    pub gross_minor: i64,
}

impl Amounts {
    /// From a price after discounts and the tax on it: the price holds the
    /// tax when `inclusive`, else the tax comes on top.
    pub fn from_price(price_minor: i64, tax_minor: i64, inclusive: bool) -> Self {
        if inclusive {
            Self {
                net_minor: price_minor - tax_minor,
                tax_minor,
                gross_minor: price_minor,
            }
        } else {
            Self {
                net_minor: price_minor,
                tax_minor,
                gross_minor: price_minor + tax_minor,
            }
        }
    }

    fn add(&mut self, other: Amounts) {
        self.net_minor += other.net_minor;
        self.tax_minor += other.tax_minor;
        self.gross_minor += other.gross_minor;
    }
}

/// The totals at one tax rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateTotal {
    /// Basis points: 1900 is 19%.
    pub rate_bps: i64,
    #[serde(flatten)]
    pub amounts: Amounts,
}

/// Sums `(rate, amounts)` per rate, highest rate first; zero lines drop out.
pub fn breakdown(lines: &[(i64, Amounts)]) -> Vec<RateTotal> {
    let mut totals: Vec<RateTotal> = Vec::new();
    for (rate, a) in lines {
        if *a == Amounts::default() {
            continue;
        }
        match totals.iter_mut().find(|t| t.rate_bps == *rate) {
            Some(t) => t.amounts.add(*a),
            None => totals.push(RateTotal {
                rate_bps: *rate,
                amounts: *a,
            }),
        }
    }
    totals.sort_by_key(|t| std::cmp::Reverse(t.rate_bps));
    totals
}

/// The sum of all rates.
pub fn total(rates: &[RateTotal]) -> Amounts {
    let mut sum = Amounts::default();
    for r in rates {
        sum.add(r.amounts);
    }
    sum
}

/// Shares a refund of `gross_minor` over the rates of the invoice it
/// credits, in proportion to their gross, and works out each part's tax.
/// At most the invoice's gross is shared.
pub fn credit_split(gross_minor: i64, rates: &[RateTotal]) -> Vec<RateTotal> {
    let weights: Vec<i64> = rates.iter().map(|r| r.amounts.gross_minor).collect();
    allocate(gross_minor, &weights)
        .into_iter()
        .zip(rates)
        .filter(|(part, _)| *part > 0)
        .map(|(part, r)| {
            // Keep the invoice's own ratio when the rate carried no tax
            // (reverse charge, exempt).
            let tax = if r.amounts.tax_minor == 0 {
                0
            } else {
                tax_on(part, r.rate_bps, true)
            };
            RateTotal {
                rate_bps: r.rate_bps,
                amounts: Amounts {
                    net_minor: part - tax,
                    tax_minor: tax,
                    gross_minor: part,
                },
            }
        })
        .collect()
}

/// An invoice number: `prefix`, the year and a five-digit sequence, e.g.
/// `INV-2026-00042`.
pub fn number(prefix: &str, year: i32, sequence: i64) -> String {
    format!("{prefix}{year}-{sequence:05}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(net: i64, tax: i64, gross: i64) -> Amounts {
        Amounts {
            net_minor: net,
            tax_minor: tax,
            gross_minor: gross,
        }
    }

    #[test]
    fn prices_split_into_net_and_tax() {
        assert_eq!(Amounts::from_price(2380, 380, true), a(2000, 380, 2380));
        assert_eq!(Amounts::from_price(2000, 380, false), a(2000, 380, 2380));
    }

    #[test]
    fn totals_per_rate() {
        let rates = breakdown(&[
            (700, a(1000, 70, 1070)),
            (1900, a(2000, 380, 2380)),
            (1900, a(416, 79, 495)),
            (0, Amounts::default()),
        ]);
        assert_eq!(
            rates,
            vec![
                RateTotal {
                    rate_bps: 1900,
                    amounts: a(2416, 459, 2875)
                },
                RateTotal {
                    rate_bps: 700,
                    amounts: a(1000, 70, 1070)
                },
            ]
        );
        assert_eq!(total(&rates), a(3416, 529, 3945));
    }

    #[test]
    fn refunds_split_over_rates() {
        let rates = vec![
            RateTotal {
                rate_bps: 1900,
                amounts: a(2416, 459, 2875),
            },
            RateTotal {
                rate_bps: 700,
                amounts: a(1000, 70, 1070),
            },
        ];
        let parts = credit_split(1000, &rates);
        assert_eq!(
            parts.iter().map(|p| p.amounts.gross_minor).sum::<i64>(),
            1000
        );
        assert_eq!(parts[0].amounts, a(613, 116, 729));
        assert_eq!(parts[1].amounts, a(253, 18, 271));
        // Never more than the invoice; reverse charge stays tax-free.
        assert_eq!(total(&credit_split(99_999, &rates)).gross_minor, 3945);
        let rc = vec![RateTotal {
            rate_bps: 0,
            amounts: a(2000, 0, 2000),
        }];
        assert_eq!(credit_split(500, &rc)[0].amounts, a(500, 0, 500));
        assert!(credit_split(0, &rates).is_empty());
    }

    #[test]
    fn numbers() {
        assert_eq!(number("INV-", 2026, 42), "INV-2026-00042");
        assert_eq!(number("", 2027, 123_456), "2027-123456");
    }
}
