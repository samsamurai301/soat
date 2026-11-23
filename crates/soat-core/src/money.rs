//! Money as an integer number of minor units (cents) plus an ISO 4217 code.
//!
//! Floats never touch prices: `19.99 EUR` is stored as `1999` and `EUR`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An ISO 4217 currency code such as `EUR`: three ASCII uppercase letters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency([u8; 3]);

/// Why a currency code was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencyError(pub String);

impl fmt::Display for CurrencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a three-letter uppercase currency code",
            self.0
        )
    }
}

impl std::error::Error for CurrencyError {}

impl Currency {
    /// Parses a code like `EUR`. Lowercase is rejected so stored codes stay canonical.
    pub fn parse(code: &str) -> Result<Self, CurrencyError> {
        let bytes = code.as_bytes();
        if bytes.len() == 3 && bytes.iter().all(u8::is_ascii_uppercase) {
            Ok(Self([bytes[0], bytes[1], bytes[2]]))
        } else {
            Err(CurrencyError(code.to_owned()))
        }
    }

    /// The code as text, e.g. `"EUR"`.
    pub fn as_str(&self) -> &str {
        // Only ASCII uppercase letters get in through `parse`.
        std::str::from_utf8(&self.0).expect("currency codes are ASCII")
    }
}

impl TryFrom<String> for Currency {
    type Error = CurrencyError;
    fn try_from(code: String) -> Result<Self, Self::Error> {
        Self::parse(&code)
    }
}

impl From<Currency> for String {
    fn from(c: Currency) -> Self {
        c.as_str().to_owned()
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Currency({})", self.as_str())
    }
}

/// An amount of money in one currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    /// Minor units: cents for EUR and USD.
    pub amount_minor: i64,
    pub currency: Currency,
}

/// Why an arithmetic operation on money failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// The two amounts are in different currencies.
    CurrencyMismatch(Currency, Currency),
    /// The result does not fit in an `i64`.
    Overflow,
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrencyMismatch(a, b) => write!(f, "cannot combine {a} with {b}"),
            Self::Overflow => f.write_str("amount out of range"),
        }
    }
}

impl std::error::Error for MoneyError {}

impl Money {
    pub fn new(amount_minor: i64, currency: Currency) -> Self {
        Self {
            amount_minor,
            currency,
        }
    }

    /// Adds two amounts of the same currency.
    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        if self.currency != other.currency {
            return Err(MoneyError::CurrencyMismatch(self.currency, other.currency));
        }
        let amount = self
            .amount_minor
            .checked_add(other.amount_minor)
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }

    /// Multiplies by a quantity, as for a cart line.
    pub fn checked_mul(self, quantity: i64) -> Result<Money, MoneyError> {
        let amount = self
            .amount_minor
            .checked_mul(quantity)
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::new(amount, self.currency))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eur() -> Currency {
        Currency::parse("EUR").unwrap()
    }

    #[test]
    fn currency_codes_must_be_three_uppercase_letters() {
        assert_eq!(eur().as_str(), "EUR");
        for bad in ["eur", "EU", "EURO", "E1R", ""] {
            assert!(Currency::parse(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn currency_round_trips_through_json() {
        let json = serde_json::to_string(&eur()).unwrap();
        assert_eq!(json, "\"EUR\"");
        assert_eq!(serde_json::from_str::<Currency>(&json).unwrap(), eur());
        assert!(serde_json::from_str::<Currency>("\"eur\"").is_err());
    }

    #[test]
    fn adding_and_multiplying_stay_exact() {
        let line = Money::new(1999, eur()).checked_mul(3).unwrap();
        assert_eq!(line.amount_minor, 5997);
        let total = line.checked_add(Money::new(3, eur())).unwrap();
        assert_eq!(total.amount_minor, 6000);
    }

    #[test]
    fn mixing_currencies_or_overflowing_is_an_error() {
        let usd = Currency::parse("USD").unwrap();
        assert_eq!(
            Money::new(1, eur()).checked_add(Money::new(1, usd)),
            Err(MoneyError::CurrencyMismatch(eur(), usd))
        );
        assert_eq!(
            Money::new(i64::MAX, eur()).checked_mul(2),
            Err(MoneyError::Overflow)
        );
    }
}
