//! Public ids: a short type prefix, an underscore and 26 characters of
//! time-ordered randomness, like `prod_01jdx3k9q8m2v4t6w7y0z1a2b3`.
//!
//! The prefix tells a person (or Claude) what an id points at, so pasting an
//! order id where a product id belongs fails clearly. The body is a ULID in
//! lowercase Crockford base32: 48 bits of milliseconds, then 80 random bits,
//! so ids sort by creation time and can be made anywhere without a database
//! round trip (imports, offline POS, plugins).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Crockford base32, lowercase: no i, l, o or u.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const BODY_LEN: usize = 26;

/// What an id points at. Declare one with [`id_kind!`](crate::id_kind).
pub trait IdKind {
    /// The prefix before the underscore, e.g. `prod`.
    const PREFIX: &'static str;
}

/// Declares an id kind and its alias:
/// `id_kind!(ProductId, Product, "prod")` gives `type ProductId = Id<Product>`.
#[macro_export]
macro_rules! id_kind {
    ($alias:ident, $kind:ident, $prefix:literal) => {
        #[doc = concat!("Marks ids with the `", $prefix, "_` prefix.")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $kind {}
        impl $crate::IdKind for $kind {
            const PREFIX: &'static str = $prefix;
        }
        #[doc = concat!("An id like `", $prefix, "_01jdx3k9q8m2v4t6w7y0z1a2b3`.")]
        pub type $alias = $crate::Id<$kind>;
    };
}

/// A typed public id. See the module docs for the format.
pub struct Id<K> {
    text: String,
    _kind: PhantomData<fn() -> K>,
}

/// Why a string is not an id of the expected kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    pub input: String,
    pub expected_prefix: &'static str,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a valid id: expected `{}_` followed by 26 characters",
            self.input, self.expected_prefix
        )
    }
}

impl std::error::Error for IdError {}

/// The last (millis, randomness) handed out, shared by every id kind.
static LAST: std::sync::Mutex<(u64, u128)> = std::sync::Mutex::new((0, 0));

/// The monotonic ULID rule: in the same (or an earlier) millisecond as the
/// last id, reuse its time and add one to its randomness.
fn next_monotonic(now: u64, random: [u8; 10]) -> (u64, [u8; 10]) {
    const MAX_RANDOM: u128 = (1 << 80) - 1;
    let fresh = random
        .iter()
        .fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (millis, bits) = if now > last.0 {
        (now, fresh)
    } else if last.1 < MAX_RANDOM {
        (last.0, last.1 + 1)
    } else {
        // 2^80 ids in one millisecond: borrow the next one.
        (last.0 + 1, fresh)
    };
    *last = (millis, bits);
    let bytes = bits.to_be_bytes();
    let mut out = [0u8; 10];
    out.copy_from_slice(&bytes[6..]);
    (millis, out)
}

impl<K: IdKind> Id<K> {
    /// A new id for now. Ids made by this process only ever grow, even
    /// within one millisecond, so sorting by id is creation order.
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut random = [0u8; 10];
        getrandom::getrandom(&mut random).expect("the OS random source is available");
        let (millis, random) = next_monotonic(now, random);
        Self::from_parts(millis, random)
    }

    /// Builds an id from a millisecond timestamp and 80 random bits.
    pub fn from_parts(millis: u64, random: [u8; 10]) -> Self {
        // 128 bits: 48 of time, 80 of randomness, written as 26 base32 digits.
        let mut value = u128::from(millis & 0xFFFF_FFFF_FFFF) << 80;
        for (i, byte) in random.iter().enumerate() {
            value |= u128::from(*byte) << (72 - 8 * i);
        }
        let mut text = String::with_capacity(K::PREFIX.len() + 1 + BODY_LEN);
        text.push_str(K::PREFIX);
        text.push('_');
        for i in (0..BODY_LEN).rev() {
            let digit = (value >> (5 * i)) & 0x1F;
            text.push(ALPHABET[digit as usize] as char);
        }
        Self {
            text,
            _kind: PhantomData,
        }
    }

    /// Parses an id, checking its prefix and body.
    pub fn parse(input: &str) -> Result<Self, IdError> {
        let err = || IdError {
            input: input.to_owned(),
            expected_prefix: K::PREFIX,
        };
        let body = input
            .strip_prefix(K::PREFIX)
            .and_then(|rest| rest.strip_prefix('_'))
            .ok_or_else(err)?;
        let valid = body.len() == BODY_LEN
            // The first digit holds only 3 bits, so it is at most 7.
            && body.as_bytes()[0] <= b'7'
            && body.bytes().all(|b| ALPHABET.contains(&b));
        if !valid {
            return Err(err());
        }
        Ok(Self {
            text: input.to_owned(),
            _kind: PhantomData,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl<K: IdKind> Default for Id<K> {
    fn default() -> Self {
        Self::new()
    }
}

// Written out by hand so `K` needs none of these traits.
impl<K> Clone for Id<K> {
    fn clone(&self) -> Self {
        Self {
            text: self.text.clone(),
            _kind: PhantomData,
        }
    }
}

impl<K> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl<K> Eq for Id<K> {}

impl<K> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

impl<K> Hash for Id<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl<K> fmt::Display for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl<K> fmt::Debug for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.text)
    }
}

impl<K> Serialize for Id<K> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text)
    }
}

impl<'de, K: IdKind> Deserialize<'de> for Id<K> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::id_kind!(TestId, Test, "test");
    crate::id_kind!(OtherId, Other, "othr");

    #[test]
    fn new_ids_parse_back_and_carry_their_prefix() {
        let id = TestId::new();
        assert!(id.as_str().starts_with("test_"));
        assert_eq!(id.as_str().len(), "test_".len() + 26);
        assert_eq!(TestId::parse(id.as_str()), Ok(id));
    }

    #[test]
    fn known_parts_give_a_known_id() {
        let id = TestId::from_parts(0, [0; 10]);
        assert_eq!(id.as_str(), "test_00000000000000000000000000");
        let max = TestId::from_parts(u64::MAX, [0xFF; 10]);
        assert_eq!(max.as_str(), "test_7zzzzzzzzzzzzzzzzzzzzzzzzz");
    }

    #[test]
    fn ids_sort_by_time() {
        let earlier = TestId::from_parts(1_700_000_000_000, [0xFF; 10]);
        let later = TestId::from_parts(1_700_000_000_001, [0; 10]);
        assert!(earlier < later);
    }

    #[test]
    fn wrong_prefix_or_body_is_rejected() {
        let id = TestId::new();
        assert!(OtherId::parse(id.as_str()).is_err());
        for bad in [
            "",
            "test_",
            "test01jdx3k9q8m2v4t6w7y0z1a2b3",
            "test_01jdx3k9q8m2v4t6w7y0z1a2b",
            "test_01JDX3K9Q8M2V4T6W7Y0Z1A2B3",
            "test_01jdx3k9q8m2v4t6w7y0z1a2bu",
            "test_81jdx3k9q8m2v4t6w7y0z1a2b3",
        ] {
            assert!(TestId::parse(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn ids_round_trip_through_json() {
        let id = TestId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<TestId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<TestId>("\"nope\"").is_err());
    }

    #[test]
    fn ids_made_in_a_burst_keep_their_order() {
        let ids: Vec<String> = (0..2000).map(|_| TestId::new().to_string()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn a_clock_going_back_does_not_reorder() {
        // A minute ahead, with the randomness exhausted: later ids borrow
        // the next millisecond. (Realistic times: other tests share LAST.)
        let ahead = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000;
        let (t1, r1) = next_monotonic(ahead, [0xff; 10]);
        let (t2, r2) = next_monotonic(5, [0; 10]);
        assert!((t2, r2) > (t1, r1));
    }
}
