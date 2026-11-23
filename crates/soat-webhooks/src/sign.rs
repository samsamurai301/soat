//! Signing deliveries so receivers know they come from this shop.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The `soat-signature` header value for `body` sent at `timestamp` (unix
/// seconds): `t=<timestamp>,v1=<hex HMAC-SHA256 of "<timestamp>.<body>">`.
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    format!("t={timestamp},v1={}", hex(&mac(secret, timestamp, body)))
}

/// Checks a `soat-signature` header against `body`. Receivers should also
/// reject timestamps older than a few minutes.
pub fn verify(secret: &str, header: &str, body: &[u8]) -> bool {
    let mut timestamp = None;
    let mut given = None;
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => timestamp = v.parse::<u64>().ok(),
            Some(("v1", v)) => given = Some(v),
            _ => {}
        }
    }
    match (timestamp, given) {
        (Some(t), Some(given)) => {
            zapline::auth::constant_time_eq(hex(&mac(secret, t, body)), given)
        }
        _ => false,
    }
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
    fn a_signature_verifies_only_for_its_secret_and_body() {
        let header = sign("s3cret", 1_700_000_000, b"{}");
        assert!(header.starts_with("t=1700000000,v1="));
        assert!(verify("s3cret", &header, b"{}"));
        assert!(!verify("other", &header, b"{}"));
        assert!(!verify("s3cret", &header, b"{ }"));
        assert!(!verify("s3cret", "garbage", b"{}"));
    }

    #[test]
    fn matches_a_known_vector() {
        // HMAC-SHA256(key "key", "1.body"), computed with Python's hmac.
        assert_eq!(
            sign("key", 1, b"body"),
            "t=1,v1=91b5374b153842ad05b2c4eab9349b8321b14703165bd3fb8b034dfb8be98ae5"
        );
    }
}
