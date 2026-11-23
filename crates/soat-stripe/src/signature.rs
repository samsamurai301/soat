//! Checking the `Stripe-Signature` header of webhook events.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// How old an event may be, in seconds.
pub const TOLERANCE: u64 = 300;

/// The header Stripe would send for `body` at `timestamp`.
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    format!("t={timestamp},v1={}", hex(&mac(secret, timestamp, body)))
}

/// Whether `header` signs `body` with `secret` within [`TOLERANCE`] of
/// `now` (unix seconds). Any of several `v1` signatures may match, as
/// during a secret roll.
pub fn verify(secret: &str, header: &str, body: &[u8], now: u64) -> bool {
    let mut timestamp = None;
    let mut given = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => timestamp = v.parse::<u64>().ok(),
            Some(("v1", v)) => given.push(v),
            _ => {}
        }
    }
    let Some(t) = timestamp else {
        return false;
    };
    if t.abs_diff(now) > TOLERANCE {
        return false;
    }
    let expected = hex(&mac(secret, t, body));
    given
        .iter()
        .any(|g| zapline::auth::constant_time_eq(expected.as_str(), g))
}

fn mac(secret: &str, timestamp: u64, body: &[u8]) -> Vec<u8> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tolerance() {
        let h = sign("whsec", 1000, b"{}");
        assert!(verify("whsec", &h, b"{}", 1000));
        assert!(verify("whsec", &h, b"{}", 1000 + TOLERANCE));
        assert!(!verify("whsec", &h, b"{}", 1001 + TOLERANCE));
        assert!(!verify("other", &h, b"{}", 1000));
        assert!(!verify("whsec", &h, b"{ }", 1000));
        let rolled = format!("{h},v1=deadbeef");
        assert!(verify("whsec", &rolled, b"{}", 1000));
        assert!(!verify("whsec", "v1=abc", b"{}", 1000));
    }
}
