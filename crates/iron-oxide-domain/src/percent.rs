//! Percentages, e.g. of a training max or of a deload.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Quantity, ValueError};
use crate::weight::Weight;

const BASIS_POINTS_PER_PERCENT: u32 = 100;
const MAX_BASIS_POINTS: u32 = 1_000 * BASIS_POINTS_PER_PERCENT;

/// A percentage between 0 % and 1 000 %, stored exactly in hundredths of a percent (basis points).
///
/// Serializes as a plain number of percent: `72.5` means 72.5 %.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Percent(u32);

impl Percent {
    /// 0 %.
    pub const ZERO: Self = Self(0);
    /// 100 %.
    pub const HUNDRED: Self = Self(100 * BASIS_POINTS_PER_PERCENT);
    /// 1 000 %, the largest accepted value.
    pub const MAX: Self = Self(MAX_BASIS_POINTS);

    /// Builds a percentage from a number of percent (`72.5` for 72.5 %), rounded to the nearest
    /// hundredth of a percent. `-0.0` is accepted as zero.
    ///
    /// # Errors
    /// [`ValueError::NotFinite`] for NaN or infinities, [`ValueError::Negative`] below zero, and
    /// [`ValueError::TooLarge`] above 1 000 %.
    pub fn new(percent: f64) -> Result<Self, ValueError> {
        if !percent.is_finite() {
            return Err(ValueError::NotFinite {
                quantity: Quantity::Percent,
            });
        }
        if percent < 0.0 {
            return Err(ValueError::Negative {
                quantity: Quantity::Percent,
                value: percent,
            });
        }
        let basis_points = (percent * f64::from(BASIS_POINTS_PER_PERCENT)).round();
        if basis_points > f64::from(MAX_BASIS_POINTS) {
            return Err(too_large());
        }
        // In range [0, MAX_BASIS_POINTS] and integral, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Self(basis_points as u32))
    }

    /// Builds a percentage from hundredths of a percent (`7250` for 72.5 %).
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] above 1 000 %.
    pub const fn from_basis_points(basis_points: u32) -> Result<Self, ValueError> {
        if basis_points > MAX_BASIS_POINTS {
            Err(too_large())
        } else {
            Ok(Self(basis_points))
        }
    }

    /// Hundredths of a percent.
    #[must_use]
    pub const fn basis_points(self) -> u32 {
        self.0
    }

    /// The number of percent (`72.5`).
    #[must_use]
    pub fn as_percent(self) -> f64 {
        f64::from(self.0) / f64::from(BASIS_POINTS_PER_PERCENT)
    }

    /// The ratio (`0.725`).
    #[must_use]
    pub fn as_fraction(self) -> f64 {
        f64::from(self.0) / f64::from(100 * BASIS_POINTS_PER_PERCENT)
    }

    /// This percentage of `weight`, rounded to the nearest nanogram. Round the result to a plate
    /// increment with [`Weight::round_to`].
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] when the result exceeds [`Weight::MAX`].
    pub fn of(self, weight: Weight) -> Result<Weight, ValueError> {
        let denominator = u128::from(100 * BASIS_POINTS_PER_PERCENT);
        // At most 2e15 * 1e5 = 2e20, far below u128::MAX; the quotient (at most 2e16) fits a u64.
        let scaled = u128::from(weight.as_nanograms()) * u128::from(self.0);
        let nanograms = (scaled + denominator / 2) / denominator;
        u64::try_from(nanograms)
            .map_err(|_| crate::weight::too_large())
            .and_then(Weight::from_nanograms)
    }
}

const fn too_large() -> ValueError {
    ValueError::TooLarge {
        quantity: Quantity::Percent,
        max: "1000%",
    }
}

impl fmt::Display for Percent {
    /// `72.5%`, `100%`, `33.33%`. Width, fill and alignment are honoured.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / BASIS_POINTS_PER_PERCENT;
        let fraction = self.0 % BASIS_POINTS_PER_PERCENT;
        let text = if fraction == 0 {
            format!("{whole}%")
        } else if fraction.is_multiple_of(10) {
            format!("{whole}.{}%", fraction / 10)
        } else {
            format!("{whole}.{fraction:02}%")
        };
        crate::display::pad(f, &text)
    }
}

impl Serialize for Percent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_percent())
    }
}

impl<'de> Deserialize<'de> for Percent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let percent = f64::deserialize(deserializer)?;
        Self::new(percent).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn pct(value: f64) -> Percent {
        Percent::new(value).unwrap()
    }

    #[test]
    fn builds_and_reads_back() {
        let p = pct(72.5);
        assert_eq!(p.basis_points(), 7_250);
        assert_eq!(p.as_percent(), 72.5);
        assert_eq!(p.as_fraction(), 0.725);
        assert_eq!(Percent::from_basis_points(7_250).unwrap(), p);
        assert_eq!(pct(100.0), Percent::HUNDRED);
        assert_eq!(pct(0.0), Percent::ZERO);
        assert_eq!(pct(-0.0), Percent::ZERO);
        assert_eq!(Percent::default(), Percent::ZERO);
        assert_eq!(pct(33.333_3).basis_points(), 3_333);
        assert_eq!(pct(0.005).basis_points(), 1);
    }

    #[test]
    fn rejects_invalid_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                Percent::new(value).unwrap_err().to_string(),
                "percent must be a finite number"
            );
        }
        assert_eq!(
            Percent::new(-10.0).unwrap_err(),
            ValueError::Negative {
                quantity: Quantity::Percent,
                value: -10.0
            }
        );
        for value in [-0.5, -1e-9] {
            assert_eq!(
                Percent::new(value).unwrap_err(),
                ValueError::Negative {
                    quantity: Quantity::Percent,
                    value
                }
            );
        }
        assert_eq!(pct(1_000.0), Percent::MAX);
        assert_eq!(
            Percent::new(1_000.01).unwrap_err().to_string(),
            "percent must be at most 1000%"
        );
        assert!(Percent::from_basis_points(100_000).is_ok());
        assert!(Percent::from_basis_points(100_001).is_err());
    }

    #[test]
    fn of_weight() {
        let tm = Weight::from_kg(100.0).unwrap();
        assert_eq!(pct(72.5).of(tm).unwrap(), Weight::from_kg(72.5).unwrap());
        assert_eq!(Percent::HUNDRED.of(tm).unwrap(), tm);
        assert_eq!(Percent::ZERO.of(tm).unwrap(), Weight::ZERO);
        assert_eq!(
            pct(90.0).of(Weight::from_lb(250.0).unwrap()).unwrap(),
            Weight::from_lb(225.0).unwrap()
        );
        // 1/3 of a nanogram rounds to zero, 2/3 rounds to one.
        let one = Weight::from_nanograms(1).unwrap();
        assert_eq!(pct(33.33).of(one).unwrap(), Weight::ZERO);
        assert_eq!(pct(66.67).of(one).unwrap(), one);
        assert_eq!(pct(50.0).of(one).unwrap(), one);
        assert!(pct(100.01).of(Weight::MAX).is_err());
        assert!(Percent::MAX.of(Weight::MAX).is_err());
        assert_eq!(
            pct(50.0).of(Weight::MAX).unwrap(),
            Weight::from_kg(1_000.0).unwrap()
        );
    }

    #[test]
    fn display() {
        assert_eq!(pct(72.5).to_string(), "72.5%");
        assert_eq!(pct(100.0).to_string(), "100%");
        assert_eq!(pct(33.33).to_string(), "33.33%");
        assert_eq!(pct(2.05).to_string(), "2.05%");
        assert_eq!(pct(0.0).to_string(), "0%");
    }

    #[test]
    fn display_honours_width_fill_and_alignment() {
        assert_eq!(format!("{:>7}|", pct(72.5)), "  72.5%|");
        assert_eq!(format!("{:<7}|", pct(72.5)), "72.5%  |");
        assert_eq!(format!("{:^7}|", pct(100.0)), " 100%  |");
        assert_eq!(format!("{:_>6}", pct(5.0)), "____5%");
        assert_eq!(format!("{:2}", pct(33.33)), "33.33%");
    }

    #[test]
    fn serde_is_a_percent_number() {
        assert_eq!(serde_json::to_string(&pct(72.5)).unwrap(), "72.5");
        assert_eq!(serde_json::from_str::<Percent>("85").unwrap(), pct(85.0));
        assert!(serde_json::from_str::<Percent>("-5").is_err());
        assert!(serde_json::from_str::<Percent>("1000.5").is_err());
    }

    proptest! {
        #[test]
        fn serde_round_trips_exactly(basis_points in 0..=MAX_BASIS_POINTS) {
            let p = Percent::from_basis_points(basis_points).unwrap();
            let json = serde_json::to_string(&p).unwrap();
            prop_assert_eq!(serde_json::from_str::<Percent>(&json).unwrap(), p);
            prop_assert_eq!(Percent::new(p.as_percent()).unwrap(), p);
        }
    }
}
