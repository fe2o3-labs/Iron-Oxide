//! Repetition counts.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Quantity, ValueError};

/// A number of repetitions. Zero is allowed (a failed attempt is logged as 0 reps); rules that need
/// at least one rep, such as 1RM estimates, check [`Reps::is_zero`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Reps(u16);

impl Reps {
    /// No repetitions.
    pub const ZERO: Self = Self(0);
    /// The largest count.
    pub const MAX: Self = Self(u16::MAX);

    /// Wraps a count. Every `u16` is valid.
    #[must_use]
    pub const fn new(count: u16) -> Self {
        Self(count)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Whether no repetitions were done.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl From<u16> for Reps {
    fn from(count: u16) -> Self {
        Self(count)
    }
}

impl From<Reps> for u16 {
    fn from(reps: Reps) -> Self {
        reps.0
    }
}

impl TryFrom<i64> for Reps {
    type Error = ValueError;

    /// Converts a count from a wider signed integer (form input, database column).
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            #[allow(clippy::cast_precision_loss)] // only used for the error message
            return Err(ValueError::Negative {
                quantity: Quantity::Reps,
                value: value as f64,
            });
        }
        u16::try_from(value)
            .map(Self)
            .map_err(|_| ValueError::TooLarge {
                quantity: Quantity::Reps,
                max: "65535",
            })
    }
}

impl fmt::Display for Reps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps() {
        let reps = Reps::new(8);
        assert_eq!(reps.get(), 8);
        assert_eq!(u16::from(reps), 8);
        assert_eq!(Reps::from(8_u16), reps);
        assert_eq!(reps.to_string(), "8");
        assert!(!reps.is_zero());
        assert!(Reps::ZERO.is_zero());
        assert_eq!(Reps::default(), Reps::ZERO);
        assert!(Reps::new(5) < Reps::new(12));
    }

    #[test]
    fn try_from_i64_boundaries() {
        assert_eq!(Reps::try_from(0_i64).unwrap(), Reps::ZERO);
        assert_eq!(Reps::try_from(65_535_i64).unwrap(), Reps::MAX);
        assert_eq!(
            Reps::try_from(65_536_i64).unwrap_err().to_string(),
            "reps must be at most 65535"
        );
        assert_eq!(
            Reps::try_from(-1_i64).unwrap_err(),
            ValueError::Negative {
                quantity: Quantity::Reps,
                value: -1.0
            }
        );
    }

    #[test]
    fn serde_is_a_bare_integer() {
        assert_eq!(serde_json::to_string(&Reps::new(10)).unwrap(), "10");
        assert_eq!(serde_json::from_str::<Reps>("10").unwrap(), Reps::new(10));
        assert!(serde_json::from_str::<Reps>("-1").is_err());
        assert!(serde_json::from_str::<Reps>("65536").is_err());
        assert!(serde_json::from_str::<Reps>("2.5").is_err());
    }
}
