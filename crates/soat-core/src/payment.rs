//! Payment states and the moves between them.

use std::fmt;
use std::str::FromStr;

/// Where a payment stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaymentStatus {
    /// Started; waiting for the buyer or the provider.
    Pending,
    /// The buyer must act, e.g. 3-D Secure or a redirect.
    RequiresAction,
    /// Held on the buyer's card; capture to collect.
    Authorized,
    /// Collected.
    Paid,
    Failed,
    Canceled,
    /// Some of a paid amount was returned.
    PartiallyRefunded,
    /// All of it was returned.
    Refunded,
}

use PaymentStatus::*;

impl PaymentStatus {
    pub const ALL: [PaymentStatus; 8] = [
        Pending,
        RequiresAction,
        Authorized,
        Paid,
        Failed,
        Canceled,
        PartiallyRefunded,
        Refunded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Pending => "pending",
            RequiresAction => "requires_action",
            Authorized => "authorized",
            Paid => "paid",
            Failed => "failed",
            Canceled => "canceled",
            PartiallyRefunded => "partially_refunded",
            Refunded => "refunded",
        }
    }

    /// Whether nothing more can happen to it.
    pub fn is_final(self) -> bool {
        matches!(self, Failed | Canceled | Refunded)
    }

    /// Whether money was collected (and some may be refunded).
    pub fn is_collected(self) -> bool {
        matches!(self, Paid | PartiallyRefunded | Refunded)
    }

    /// Whether a payment may move from `self` to `next`. Staying put is
    /// allowed (providers repeat themselves).
    pub fn can_become(self, next: PaymentStatus) -> bool {
        if self == next {
            return true;
        }
        match self {
            Pending => matches!(next, RequiresAction | Authorized | Paid | Failed | Canceled),
            RequiresAction => matches!(next, Pending | Authorized | Paid | Failed | Canceled),
            Authorized => matches!(next, Paid | Failed | Canceled),
            Paid => matches!(next, PartiallyRefunded | Refunded),
            PartiallyRefunded => matches!(next, Refunded),
            Failed | Canceled | Refunded => false,
        }
    }

    /// The status after `refunded` of `amount` has been returned.
    pub fn after_refunds(amount: i64, refunded: i64) -> PaymentStatus {
        if refunded <= 0 {
            Paid
        } else if refunded >= amount {
            Refunded
        } else {
            PartiallyRefunded
        }
    }
}

impl fmt::Display for PaymentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PaymentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| format!("unknown payment status {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in PaymentStatus::ALL {
            assert_eq!(s.as_str().parse::<PaymentStatus>(), Ok(s));
        }
        assert!("paid!".parse::<PaymentStatus>().is_err());
    }

    #[test]
    fn moves() {
        assert!(Pending.can_become(Paid));
        assert!(Pending.can_become(Pending));
        assert!(Authorized.can_become(Paid));
        assert!(Paid.can_become(Refunded));
        assert!(!Paid.can_become(Canceled));
        assert!(!Failed.can_become(Paid));
        assert!(!Refunded.can_become(Paid));
        assert!(!PartiallyRefunded.can_become(Paid));
        for s in PaymentStatus::ALL {
            if s.is_final() {
                assert!(PaymentStatus::ALL
                    .iter()
                    .all(|n| *n == s || !s.can_become(*n)));
            }
        }
    }

    #[test]
    fn refunds() {
        assert_eq!(PaymentStatus::after_refunds(1000, 0), Paid);
        assert_eq!(PaymentStatus::after_refunds(1000, 1), PartiallyRefunded);
        assert_eq!(PaymentStatus::after_refunds(1000, 1000), Refunded);
    }
}
