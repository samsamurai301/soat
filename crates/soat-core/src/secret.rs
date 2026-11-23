//! Secrets kept in the database, such as a shop's own API keys, sealed
//! with the server's secret (AES-256-GCM). Each sealed value is bound to a
//! context, usually its shop's id, so it cannot be moved to another row.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

/// The shortest server secret accepted.
pub const MIN_SECRET_LEN: usize = 32;

const NONCE_LEN: usize = 12;

/// Why a server secret was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

/// Seals and opens values with a key made from the server's secret.
#[derive(Clone)]
pub struct Sealer {
    cipher: Aes256Gcm,
}

impl fmt::Debug for Sealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sealer(..)")
    }
}

impl Sealer {
    /// A sealer keyed by SHA-256 of `secret`.
    ///
    /// # Errors
    ///
    /// When `secret` is shorter than [`MIN_SECRET_LEN`] bytes.
    pub fn from_secret(secret: &str) -> Result<Self, SecretError> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(SecretError(format!(
                "the server secret needs at least {MIN_SECRET_LEN} characters"
            )));
        }
        let key = Sha256::digest(secret.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| SecretError("the key has the wrong length".into()))?;
        Ok(Self { cipher })
    }

    /// `value` sealed for `context`: a random nonce, then the ciphertext.
    ///
    /// # Panics
    ///
    /// When the system has no randomness, which leaves nothing safe to do.
    pub fn seal(&self, value: &[u8], context: &str) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).expect("the system has randomness");
        let sealed = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: value,
                    aad: context.as_bytes(),
                },
            )
            .expect("sealing fits in memory");
        let mut out = nonce.to_vec();
        out.extend(sealed);
        out
    }

    /// The value sealed for `context`; `None` when it was sealed with
    /// another secret or context, or changed since.
    pub fn open(&self, sealed: &[u8], context: &str) -> Option<Vec<u8>> {
        if sealed.len() < NONCE_LEN {
            return None;
        }
        let (nonce, data) = sealed.split_at(NONCE_LEN);
        self.cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: data,
                    aad: context.as_bytes(),
                },
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a server secret of at least thirty-two characters";

    #[test]
    fn opens_what_it_sealed_for_the_same_context() {
        let s = Sealer::from_secret(SECRET).unwrap();
        let sealed = s.seal(b"sk-ant-123", "acme");
        assert!(!sealed.windows(10).any(|w| w == b"sk-ant-123"));
        assert_eq!(s.open(&sealed, "acme").unwrap(), b"sk-ant-123");
        // Each seal is different.
        assert_ne!(sealed, s.seal(b"sk-ant-123", "acme"));
    }

    #[test]
    fn refuses_another_context_secret_or_a_change() {
        let s = Sealer::from_secret(SECRET).unwrap();
        let sealed = s.seal(b"value", "acme");
        assert!(s.open(&sealed, "globex").is_none());
        let other = Sealer::from_secret(&format!("{SECRET}!")).unwrap();
        assert!(other.open(&sealed, "acme").is_none());
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(s.open(&changed, "acme").is_none());
        assert!(s.open(b"short", "acme").is_none());
    }

    #[test]
    fn wants_a_long_secret() {
        assert!(Sealer::from_secret("short").is_err());
        assert_eq!(
            format!("{:?}", Sealer::from_secret(SECRET).unwrap()),
            "Sealer(..)"
        );
    }
}
