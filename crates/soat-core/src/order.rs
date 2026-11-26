//! Orders: how far an order is paid, from its payments.

use crate::payment::PaymentStatus;

/// One payment of an order, as far as paying goes.
#[derive(Debug, Clone, Copy)]
pub struct PaidPart {
    pub status: PaymentStatus,
    pub amount_minor: i64,
    pub refunded_minor: i64,
}

/// How far an order of `total_minor` is paid by `payments`:
///
/// - `refunded`: money came in and all of it went back;
/// - `partially_refunded`: some went back, some is kept;
/// - `paid`: what is kept covers the total;
/// - `partially_paid`: some is kept, not the total;
/// - `authorized`: held on cards for the total, not collected yet;
/// - `pending`: nothing yet.
pub fn financial_status(total_minor: i64, payments: &[PaidPart]) -> &'static str {
    let mut kept = 0i64;
    let mut refunded = 0i64;
    let mut held = 0i64;
    for p in payments {
        if p.status.is_collected() {
            kept = kept.saturating_add(p.amount_minor - p.refunded_minor);
            refunded = refunded.saturating_add(p.refunded_minor);
        } else if p.status == PaymentStatus::Authorized {
            held = held.saturating_add(p.amount_minor);
        }
    }
    if refunded > 0 {
        if kept <= 0 {
            "refunded"
        } else {
            "partially_refunded"
        }
    } else if kept >= total_minor && kept > 0 {
        "paid"
    } else if kept > 0 {
        "partially_paid"
    } else if held >= total_minor && held > 0 {
        "authorized"
    } else {
        "pending"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PaymentStatus::*;

    fn part(status: PaymentStatus, amount: i64, refunded: i64) -> PaidPart {
        PaidPart {
            status,
            amount_minor: amount,
            refunded_minor: refunded,
        }
    }

    #[test]
    fn statuses() {
        assert_eq!(financial_status(100, &[]), "pending");
        assert_eq!(financial_status(100, &[part(Failed, 100, 0)]), "pending");
        assert_eq!(
            financial_status(100, &[part(Authorized, 100, 0)]),
            "authorized"
        );
        assert_eq!(financial_status(100, &[part(Paid, 100, 0)]), "paid");
        assert_eq!(
            financial_status(100, &[part(Paid, 40, 0), part(Pending, 60, 0)]),
            "partially_paid"
        );
        assert_eq!(
            financial_status(100, &[part(Paid, 40, 0), part(Paid, 60, 0)]),
            "paid"
        );
        assert_eq!(
            financial_status(100, &[part(PartiallyRefunded, 100, 30)]),
            "partially_refunded"
        );
        assert_eq!(
            financial_status(100, &[part(Refunded, 100, 100)]),
            "refunded"
        );
    }
}
