//! Units a weight can be entered in and shown in.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::ValueError;

/// A mass unit. Weights are always stored in kilograms (see [`Weight`](crate::Weight)); the unit only
/// matters when a value enters or leaves the domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    /// Kilograms.
    Kg,
    /// Avoirdupois pounds, defined as exactly 0.453 592 37 kg.
    Lb,
}

impl Unit {
    /// Both units, for pickers.
    pub const ALL: [Self; 2] = [Self::Kg, Self::Lb];

    /// The short symbol shown after a value: `kg` or `lb`.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Kg => "kg",
            Self::Lb => "lb",
        }
    }

    /// How many storage units (nanograms) one of this unit is. Both values are exact integers.
    pub(crate) const fn nanograms(self) -> u64 {
        match self {
            Self::Kg => 1_000_000_000_000,
            Self::Lb => 453_592_370_000,
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

impl FromStr for Unit {
    type Err = ValueError;

    /// Accepts `kg`, `lb` and `lbs`, ignoring case and surrounding whitespace.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("kg") {
            Ok(Self::Kg)
        } else if trimmed.eq_ignore_ascii_case("lb") || trimmed.eq_ignore_ascii_case("lbs") {
            Ok(Self::Lb)
        } else {
            Err(ValueError::UnknownUnit(s.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_and_display() {
        assert_eq!(Unit::Kg.symbol(), "kg");
        assert_eq!(Unit::Lb.symbol(), "lb");
        assert_eq!(Unit::Kg.to_string(), "kg");
        assert_eq!(Unit::Lb.to_string(), "lb");
        assert_eq!(Unit::ALL, [Unit::Kg, Unit::Lb]);
    }

    #[test]
    fn pound_is_exactly_0_45359237_kg() {
        assert_eq!(
            u128::from(Unit::Lb.nanograms()) * 100_000_000,
            45_359_237 * u128::from(Unit::Kg.nanograms())
        );
    }

    #[test]
    fn parses_known_spellings() {
        assert_eq!("kg".parse::<Unit>().unwrap(), Unit::Kg);
        assert_eq!(" KG ".parse::<Unit>().unwrap(), Unit::Kg);
        assert_eq!("lb".parse::<Unit>().unwrap(), Unit::Lb);
        assert_eq!("Lbs".parse::<Unit>().unwrap(), Unit::Lb);
    }

    #[test]
    fn rejects_unknown_units() {
        let err = "stone".parse::<Unit>().unwrap_err();
        assert_eq!(err, ValueError::UnknownUnit("stone".to_owned()));
        assert_eq!(
            err.to_string(),
            "unknown unit `stone` (expected `kg` or `lb`)"
        );
        assert!("".parse::<Unit>().is_err());
    }

    #[test]
    fn serde_uses_lowercase_symbols() {
        assert_eq!(serde_json::to_string(&Unit::Kg).unwrap(), "\"kg\"");
        assert_eq!(serde_json::to_string(&Unit::Lb).unwrap(), "\"lb\"");
        assert_eq!(serde_json::from_str::<Unit>("\"lb\"").unwrap(), Unit::Lb);
        assert!(serde_json::from_str::<Unit>("\"Kg\"").is_err());
    }
}
