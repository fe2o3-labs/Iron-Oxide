//! A non-negative load, stored exactly. See [`Weight`] for the representation.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Quantity, ValueError};
use crate::units::Unit;

const NANOGRAMS_PER_KG: u64 = 1_000_000_000_000;
const MAX_KG: u64 = 2_000;
const MAX_NANOGRAMS: u64 = MAX_KG * NANOGRAMS_PER_KG;
/// Beyond 9 decimals the lb and kg values are already exact; more digits would be noise.
const MAX_FORMAT_DECIMALS: u8 = 9;
/// Decimals shown by [`WeightDisplay`] when no precision is given: enough for 1.25 kg plates.
const DEFAULT_DISPLAY_DECIMALS: u8 = 2;

/// How to round a weight to an increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rounding {
    /// To the closest multiple; an exact halfway value rounds up.
    Nearest,
    /// To the largest multiple not above the weight.
    Down,
    /// To the smallest multiple not below the weight.
    Up,
}

/// A non-negative weight (a load on the bar, a plate, an increment), stored exactly in kilograms.
///
/// # Representation
///
/// A weight is an integer number of **nanograms** (10⁻¹² kg) in a `u64`, capped at [`Weight::MAX`].
///
/// - The pound is defined as exactly 0.453 592 37 kg = 453 592 370 000 ng, so one pound is a whole
///   number of storage units. Every kg value with up to 9 decimals and every lb value with up to 4
///   decimals (45 lb bar, 1.25 lb and 0.25 lb change plates, ...) is stored with no rounding at all.
/// - Adding plates, comparing loads and rounding to an increment are exact integer operations in both
///   units: 45 lb + 2 × 45 lb is exactly 135 lb, and 1.25 kg plates add up exactly. A float or an
///   integer number of grams would make lb loads drift by fractions of a gram and break equality.
/// - `Eq`, `Ord` and `Hash` are real, so weights can be compared for PRs and used as map keys.
///
/// Values enter and leave through `f64` in a chosen [`Unit`]. Input is rounded exactly to the nearest
/// nanogram (computed from the bits of the `f64`, halfway rounds up), which is far below anything a
/// scale or a plate can resolve.
///
/// # Serde
///
/// A weight serializes as a plain JSON number of **kilograms** (`102.5`). The cap of 2 000 kg keeps
/// every nanogram value below 2⁵¹, so the kg `f64` always converts back to the same integer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Weight(u64);

impl Weight {
    /// No weight.
    pub const ZERO: Self = Self(0);
    /// The heaviest representable weight: 2 000 kg. This is a load, not a training volume; tonnage
    /// sums belong in their own type.
    pub const MAX: Self = Self(MAX_NANOGRAMS);

    /// Converts a value in `unit`, rounding exactly to the nearest nanogram (halfway rounds up).
    ///
    /// `-0.0` is accepted as zero.
    ///
    /// # Errors
    /// [`ValueError::NotFinite`] for NaN or infinities, [`ValueError::Negative`] below zero, and
    /// [`ValueError::TooLarge`] above [`Weight::MAX`].
    pub fn new(value: f64, unit: Unit) -> Result<Self, ValueError> {
        if !value.is_finite() {
            return Err(ValueError::NotFinite {
                quantity: Quantity::Weight,
            });
        }
        if value < 0.0 {
            return Err(ValueError::Negative {
                quantity: Quantity::Weight,
                value,
            });
        }
        match nearest_nanograms(value, unit.nanograms()) {
            Some(nanograms) if nanograms <= MAX_NANOGRAMS => Ok(Self(nanograms)),
            _ => Err(too_large()),
        }
    }

    /// Shorthand for [`Weight::new`] in kilograms.
    ///
    /// # Errors
    /// See [`Weight::new`].
    pub fn from_kg(kg: f64) -> Result<Self, ValueError> {
        Self::new(kg, Unit::Kg)
    }

    /// Shorthand for [`Weight::new`] in pounds.
    ///
    /// # Errors
    /// See [`Weight::new`].
    pub fn from_lb(lb: f64) -> Result<Self, ValueError> {
        Self::new(lb, Unit::Lb)
    }

    /// Builds a weight from its exact storage value, e.g. a database column.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] above [`Weight::MAX`].
    pub const fn from_nanograms(nanograms: u64) -> Result<Self, ValueError> {
        if nanograms > MAX_NANOGRAMS {
            Err(too_large())
        } else {
            Ok(Self(nanograms))
        }
    }

    /// The exact storage value, for persistence. Always at most 2 × 10¹⁵, so it fits an `i64`.
    #[must_use]
    pub const fn as_nanograms(self) -> u64 {
        self.0
    }

    /// The value in `unit`, as the closest `f64`.
    #[must_use]
    pub fn value_in(self, unit: Unit) -> f64 {
        #[allow(clippy::cast_precision_loss)] // both values are below 2^53, so exact
        let (nanograms, per_unit) = (self.0 as f64, unit.nanograms() as f64);
        nanograms / per_unit
    }

    /// The value in kilograms.
    #[must_use]
    pub fn as_kg(self) -> f64 {
        self.value_in(Unit::Kg)
    }

    /// The value in pounds.
    #[must_use]
    pub fn as_lb(self) -> f64 {
        self.value_in(Unit::Lb)
    }

    /// Whether this is [`Weight::ZERO`].
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Exact sum.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] when the sum exceeds [`Weight::MAX`].
    pub const fn checked_add(self, rhs: Self) -> Result<Self, ValueError> {
        // Both are at most MAX_NANOGRAMS (2e15), so the u64 sum cannot overflow.
        Self::from_nanograms(self.0 + rhs.0)
    }

    /// Exact difference.
    ///
    /// # Errors
    /// [`ValueError::Negative`] when `rhs` is heavier than `self`.
    pub fn checked_sub(self, rhs: Self) -> Result<Self, ValueError> {
        match self.0.checked_sub(rhs.0) {
            Some(nanograms) => Ok(Self(nanograms)),
            None => Err(ValueError::Negative {
                quantity: Quantity::Weight,
                value: self.as_kg() - rhs.as_kg(),
            }),
        }
    }

    /// Difference clamped at zero.
    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    /// Distance between two weights, whichever is heavier.
    #[must_use]
    pub const fn abs_diff(self, rhs: Self) -> Self {
        Self(self.0.abs_diff(rhs.0))
    }

    /// Exact product with a count, e.g. a plate size times the number of plates.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] when the product exceeds [`Weight::MAX`].
    pub fn checked_mul(self, count: u32) -> Result<Self, ValueError> {
        // 2e15 * u32::MAX does not fit a u64, so multiply in u128 (where it cannot overflow).
        let product = u128::from(self.0) * u128::from(count);
        u64::try_from(product)
            .map_err(|_| too_large())
            .and_then(Self::from_nanograms)
    }

    /// Rounds to a multiple of `increment` (e.g. 2.5 kg or 5 lb), exactly.
    ///
    /// # Errors
    /// [`ValueError::ZeroIncrement`] when `increment` is zero, and [`ValueError::TooLarge`] when
    /// rounding up passes [`Weight::MAX`].
    pub fn round_to(self, increment: Self, rounding: Rounding) -> Result<Self, ValueError> {
        let step = increment.0;
        if step == 0 {
            return Err(ValueError::ZeroIncrement);
        }
        let remainder = self.0 % step;
        let down = self.0 - remainder;
        let round_up = match rounding {
            Rounding::Down => false,
            Rounding::Up => remainder != 0,
            // remainder >= step / 2, written without overflow or halving error
            Rounding::Nearest => remainder >= step - remainder,
        };
        if round_up {
            // down and step are both at most MAX_NANOGRAMS, so the sum cannot overflow.
            Self::from_nanograms(down + step)
        } else {
            Ok(Self(down))
        }
    }

    /// The number in `unit`, rounded half-up to at most `max_decimals` (capped at 9), without
    /// trailing zeros and without the unit: `"102.5"`, `"45"`, `"20.41"`. Uses integer arithmetic,
    /// so there are no float artefacts such as `"99.99999"`.
    #[must_use]
    pub fn format_value(self, unit: Unit, max_decimals: u8) -> String {
        let decimals = max_decimals.min(MAX_FORMAT_DECIMALS);
        let scale = 10_u128.pow(u32::from(decimals));
        let per_unit = u128::from(unit.nanograms());
        // At most 2e15 * 1e9 = 2e24, far below u128::MAX. per_unit is even, so /2 is exact.
        let scaled = (u128::from(self.0) * scale + per_unit / 2) / per_unit;
        let whole = scaled / scale;
        let fraction = scaled % scale;
        if fraction == 0 {
            return whole.to_string();
        }
        let digits = format!("{fraction:0width$}", width = usize::from(decimals));
        format!("{whole}.{}", digits.trim_end_matches('0'))
    }

    /// A displayable value with its unit symbol, e.g. `102.5 kg`.
    ///
    /// Shows up to 2 decimals by default; a format precision overrides it (`{:.1}`). Width, fill and
    /// alignment are honoured (`{:>10}`).
    #[must_use]
    pub const fn display_in(self, unit: Unit) -> WeightDisplay {
        WeightDisplay { weight: self, unit }
    }
}

/// `value × per_unit` rounded to the nearest integer (halfway rounds up), computed exactly from the
/// bits of `value`. Multiplying in `f64` first would round twice and can land 1 ng off.
///
/// `value` must be finite and non-negative. Returns `None` when the result does not fit a `u64`.
fn nearest_nanograms(value: f64, per_unit: u64) -> Option<u64> {
    const MANTISSA_BITS: u32 = 52;
    const EXPONENT_BIAS: i32 = 1_075; // 1023 + 52: value = mantissa × 2^(exponent - 1075)
    let bits = value.to_bits();
    let fraction = bits & ((1_u64 << MANTISSA_BITS) - 1);
    // The biased exponent is 11 bits, so it always fits an i32.
    let biased = i32::try_from((bits >> MANTISSA_BITS) & 0x7ff).ok()?;
    let (mantissa, exponent) = if biased == 0 {
        (fraction, 1 - EXPONENT_BIAS) // subnormal
    } else {
        (fraction | (1_u64 << MANTISSA_BITS), biased - EXPONENT_BIAS)
    };
    // mantissa < 2^53 and per_unit < 2^40, so the product fits in 93 bits.
    let product = u128::from(mantissa) * u128::from(per_unit);
    let rounded = if exponent >= 0 {
        let shift = exponent.unsigned_abs();
        if shift >= 35 {
            // product ≥ 2^52 · 2^35 > u64::MAX unless it is zero
            return if product == 0 { Some(0) } else { None };
        }
        product << shift
    } else {
        let shift = exponent.unsigned_abs();
        if shift > 94 {
            // product < 2^93, so the exact value is below 0.5
            return Some(0);
        }
        let whole = product >> shift;
        let remainder = product & ((1_u128 << shift) - 1);
        let half = 1_u128 << (shift - 1);
        if remainder >= half { whole + 1 } else { whole }
    };
    u64::try_from(rounded).ok()
}

pub(crate) const fn too_large() -> ValueError {
    ValueError::TooLarge {
        quantity: Quantity::Weight,
        max: "2000 kg",
    }
}

/// Formats a [`Weight`] in a unit. Built by [`Weight::display_in`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightDisplay {
    weight: Weight,
    unit: Unit,
}

impl fmt::Display for WeightDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decimals = f.precision().map_or(DEFAULT_DISPLAY_DECIMALS, |precision| {
            u8::try_from(precision).unwrap_or(MAX_FORMAT_DECIMALS)
        });
        let text = format!(
            "{} {}",
            self.weight.format_value(self.unit, decimals),
            self.unit.symbol()
        );
        crate::display::pad(f, &text)
    }
}

impl Serialize for Weight {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_kg())
    }
}

impl<'de> Deserialize<'de> for Weight {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kg = f64::deserialize(deserializer)?;
        Self::from_kg(kg).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NG_PER_LB: u64 = 453_592_370_000;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    #[test]
    fn kg_plates_are_exact() {
        assert_eq!(kg(1.25).as_nanograms(), 1_250_000_000_000);
        assert_eq!(kg(0.5).as_nanograms(), 500_000_000_000);
        assert_eq!(
            kg(20.0)
                .checked_add(kg(1.25).checked_mul(2).unwrap())
                .unwrap(),
            kg(22.5)
        );
        assert_eq!(kg(0.1).checked_add(kg(0.2)).unwrap(), kg(0.3));
        assert_eq!(kg(1.25).as_kg(), 1.25);
    }

    #[test]
    fn lb_plates_are_exact() {
        assert_eq!(lb(45.0).as_nanograms(), 45 * NG_PER_LB);
        assert_eq!(lb(1.25).as_nanograms(), NG_PER_LB * 5 / 4);
        assert_eq!(lb(0.25).as_nanograms(), NG_PER_LB / 4);
        let bar_and_two_plates = lb(45.0)
            .checked_add(lb(45.0).checked_mul(2).unwrap())
            .unwrap();
        assert_eq!(bar_and_two_plates, lb(135.0));
        assert_eq!(bar_and_two_plates.as_lb(), 135.0);
    }

    #[test]
    fn converts_between_units_with_the_exact_factor() {
        assert_eq!(lb(1.0), kg(0.453_592_37));
        assert_eq!(lb(100.0).as_kg(), 45.359_237);
        assert_eq!(Weight::new(1.0, Unit::Lb).unwrap(), lb(1.0));
        assert_eq!(kg(100.0).value_in(Unit::Kg), 100.0);
        assert!((kg(100.0).as_lb() - 220.462_262_184_877_6).abs() < 1e-9);
    }

    #[test]
    fn zero_and_negative_zero() {
        assert_eq!(kg(0.0), Weight::ZERO);
        assert_eq!(kg(-0.0), Weight::ZERO);
        assert_eq!(Weight::default(), Weight::ZERO);
        assert!(Weight::ZERO.is_zero());
        assert!(!kg(0.5).is_zero());
    }

    #[test]
    fn rejects_non_finite_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = Weight::from_kg(value).unwrap_err();
            assert_eq!(
                err,
                ValueError::NotFinite {
                    quantity: Quantity::Weight
                }
            );
            assert_eq!(err.to_string(), "weight must be a finite number");
        }
    }

    #[test]
    fn rejects_negative_values() {
        for value in [-1.0, -1e-9, -f64::MIN_POSITIVE] {
            let err = Weight::from_lb(value).unwrap_err();
            assert_eq!(
                err,
                ValueError::Negative {
                    quantity: Quantity::Weight,
                    value
                }
            );
        }
        assert_eq!(
            Weight::from_kg(-2.5).unwrap_err().to_string(),
            "weight must not be negative (got -2.5)"
        );
    }

    #[test]
    fn max_boundary() {
        assert_eq!(kg(2_000.0), Weight::MAX);
        assert_eq!(Weight::MAX.as_kg(), 2_000.0);
        let err = Weight::from_kg(2_000.000_001).unwrap_err();
        assert_eq!(err.to_string(), "weight must be at most 2000 kg");
        assert!(Weight::from_lb(4_410.0).is_err());
        assert!(Weight::from_lb(4_409.0).is_ok());
        assert_eq!(
            Weight::from_nanograms(Weight::MAX.as_nanograms()).unwrap(),
            Weight::MAX
        );
        assert!(Weight::from_nanograms(Weight::MAX.as_nanograms() + 1).is_err());
        assert!(Weight::from_nanograms(u64::MAX).is_err());
        assert!(i64::try_from(Weight::MAX.as_nanograms()).is_ok());
    }

    #[test]
    fn tiny_values_round_to_the_nearest_nanogram() {
        assert_eq!(kg(4e-13), Weight::ZERO);
        assert_eq!(kg(6e-13).as_nanograms(), 1);
    }

    #[test]
    fn addition_and_multiplication_are_checked() {
        assert_eq!(Weight::MAX.checked_add(Weight::ZERO).unwrap(), Weight::MAX);
        assert!(
            Weight::MAX
                .checked_add(Weight::from_nanograms(1).unwrap())
                .is_err()
        );
        assert_eq!(kg(20.0).checked_mul(0).unwrap(), Weight::ZERO);
        assert_eq!(kg(20.0).checked_mul(100).unwrap(), Weight::MAX);
        assert!(kg(20.0).checked_mul(101).is_err());
        assert!(Weight::MAX.checked_mul(u32::MAX).is_err());
    }

    #[test]
    fn subtraction() {
        assert_eq!(kg(100.0).checked_sub(kg(20.0)).unwrap(), kg(80.0));
        assert_eq!(kg(20.0).checked_sub(kg(20.0)).unwrap(), Weight::ZERO);
        assert_eq!(
            kg(20.0).checked_sub(kg(22.5)).unwrap_err(),
            ValueError::Negative {
                quantity: Quantity::Weight,
                value: -2.5
            }
        );
        assert_eq!(kg(20.0).saturating_sub(kg(22.5)), Weight::ZERO);
        assert_eq!(kg(22.5).saturating_sub(kg(20.0)), kg(2.5));
        assert_eq!(kg(20.0).abs_diff(kg(22.5)), kg(2.5));
        assert_eq!(kg(22.5).abs_diff(kg(20.0)), kg(2.5));
    }

    #[test]
    fn ordering_follows_mass_across_units() {
        assert!(lb(45.0) > kg(20.0));
        assert!(lb(44.0) < kg(20.0));
        assert_eq!(kg(20.0).max(lb(45.0)), lb(45.0));
    }

    #[test]
    fn round_to_each_mode() {
        let step = kg(2.5);
        assert_eq!(
            kg(101.3).round_to(step, Rounding::Nearest).unwrap(),
            kg(102.5)
        );
        assert_eq!(
            kg(101.2).round_to(step, Rounding::Nearest).unwrap(),
            kg(100.0)
        );
        assert_eq!(
            kg(101.25).round_to(step, Rounding::Nearest).unwrap(),
            kg(102.5)
        );
        assert_eq!(kg(101.3).round_to(step, Rounding::Down).unwrap(), kg(100.0));
        assert_eq!(kg(100.1).round_to(step, Rounding::Up).unwrap(), kg(102.5));
        for mode in [Rounding::Nearest, Rounding::Down, Rounding::Up] {
            assert_eq!(kg(100.0).round_to(step, mode).unwrap(), kg(100.0));
            assert_eq!(Weight::ZERO.round_to(step, mode).unwrap(), Weight::ZERO);
        }
    }

    #[test]
    fn round_to_lb_increments_is_exact() {
        assert_eq!(
            lb(137.0).round_to(lb(5.0), Rounding::Nearest).unwrap(),
            lb(135.0)
        );
        assert_eq!(
            lb(137.5).round_to(lb(5.0), Rounding::Nearest).unwrap(),
            lb(140.0)
        );
        // 100 kg is 220.46 lb.
        let rounded = kg(100.0).round_to(lb(1.0), Rounding::Nearest).unwrap();
        assert_eq!(rounded, lb(220.0));
        assert_eq!(rounded.display_in(Unit::Lb).to_string(), "220 lb");
    }

    #[test]
    fn round_to_rejects_zero_increment_and_overflow() {
        assert_eq!(
            kg(100.0).round_to(Weight::ZERO, Rounding::Nearest),
            Err(ValueError::ZeroIncrement)
        );
        assert_eq!(
            ValueError::ZeroIncrement.to_string(),
            "rounding increment must be greater than zero"
        );
        assert!(kg(1_999.0).round_to(kg(3.0), Rounding::Up).is_err());
        assert_eq!(
            kg(1_999.0).round_to(kg(3.0), Rounding::Down).unwrap(),
            kg(1_998.0)
        );
        assert_eq!(
            Weight::MAX.round_to(Weight::MAX, Rounding::Up).unwrap(),
            Weight::MAX
        );
    }

    #[test]
    fn format_value_trims_and_rounds_half_up() {
        assert_eq!(kg(102.5).format_value(Unit::Kg, 2), "102.5");
        assert_eq!(kg(100.0).format_value(Unit::Kg, 2), "100");
        assert_eq!(kg(1.25).format_value(Unit::Kg, 2), "1.25");
        assert_eq!(kg(1.25).format_value(Unit::Kg, 1), "1.3");
        assert_eq!(kg(1.25).format_value(Unit::Kg, 0), "1");
        assert_eq!(kg(0.005).format_value(Unit::Kg, 2), "0.01");
        assert_eq!(kg(0.004).format_value(Unit::Kg, 2), "0");
        assert_eq!(kg(99.999).format_value(Unit::Kg, 2), "100");
        assert_eq!(kg(100.0).format_value(Unit::Lb, 2), "220.46");
        assert_eq!(kg(100.0).format_value(Unit::Lb, 0), "220");
        assert_eq!(lb(45.0).format_value(Unit::Kg, 3), "20.412");
        assert_eq!(lb(45.0).format_value(Unit::Lb, 9), "45");
        assert_eq!(kg(1.0).format_value(Unit::Lb, u8::MAX), "2.204622622");
        assert_eq!(Weight::MAX.format_value(Unit::Kg, 2), "2000");
    }

    #[test]
    fn display_in_shows_the_unit_and_honours_precision() {
        assert_eq!(kg(102.5).display_in(Unit::Kg).to_string(), "102.5 kg");
        assert_eq!(lb(45.0).display_in(Unit::Kg).to_string(), "20.41 kg");
        assert_eq!(lb(45.0).display_in(Unit::Lb).to_string(), "45 lb");
        assert_eq!(format!("{:.1}", lb(45.0).display_in(Unit::Kg)), "20.4 kg");
        assert_eq!(format!("{:.0}", kg(1.25).display_in(Unit::Kg)), "1 kg");
        assert_eq!(
            format!("{:.300}", kg(1.0).display_in(Unit::Lb)),
            "2.204622622 lb"
        );
    }

    #[test]
    fn serde_uses_a_kg_number() {
        assert_eq!(serde_json::to_string(&kg(102.5)).unwrap(), "102.5");
        assert_eq!(serde_json::to_string(&Weight::ZERO).unwrap(), "0.0");
        assert_eq!(serde_json::from_str::<Weight>("60").unwrap(), kg(60.0));
        assert_eq!(serde_json::from_str::<Weight>("1.25").unwrap(), kg(1.25));
        let err = serde_json::from_str::<Weight>("-5").unwrap_err();
        assert!(err.to_string().contains("weight must not be negative"));
        assert!(serde_json::from_str::<Weight>("2000.5").is_err());
        assert!(serde_json::from_str::<Weight>("\"60\"").is_err());
        let json = serde_json::to_string(&lb(45.0)).unwrap();
        assert_eq!(serde_json::from_str::<Weight>(&json).unwrap(), lb(45.0));
    }

    #[test]
    fn rounding_serde() {
        assert_eq!(
            serde_json::to_string(&Rounding::Nearest).unwrap(),
            "\"nearest\""
        );
        assert_eq!(
            serde_json::from_str::<Rounding>("\"down\"").unwrap(),
            Rounding::Down
        );
        assert_eq!(
            serde_json::from_str::<Rounding>("\"up\"").unwrap(),
            Rounding::Up
        );
    }

    #[test]
    fn new_rounds_exactly_to_the_nearest_nanogram() {
        // Exact product is 961175748098983.4903... ng; multiplying in f64 first gave ...984.
        assert_eq!(
            Weight::from_kg(961.175_748_098_983_5)
                .unwrap()
                .as_nanograms(),
            961_175_748_098_983
        );
        // 2^-13 kg is exactly 122070312.5 ng: halfway rounds up.
        assert_eq!(kg(2_f64.powi(-13)).as_nanograms(), 122_070_313);
        assert_eq!(
            kg(2_f64.powi(-13)).as_nanograms(),
            reference_nanograms(2_f64.powi(-13), Unit::Kg)
        );
        // Subnormals and the smallest normal are far below 1 ng.
        assert_eq!(kg(f64::from_bits(1)), Weight::ZERO);
        assert_eq!(lb(f64::MIN_POSITIVE), Weight::ZERO);
        // Large finite values are rejected, not wrapped.
        assert!(Weight::from_kg(f64::MAX).is_err());
        assert!(Weight::from_lb(1e30).is_err());
        assert!(Weight::from_kg(2_f64.powi(40)).is_err());
    }

    /// Independent oracle: the exact decimal expansion of `value` (Rust prints every digit when asked
    /// for enough precision), multiplied digit by digit by the unit's nanograms, rounded half-up.
    fn reference_nanograms(value: f64, unit: Unit) -> u64 {
        let text = format!("{value:.1100}");
        let (whole, fraction) = text.split_once('.').unwrap();
        let mut digits: Vec<u32> = whole
            .chars()
            .chain(fraction.chars())
            .map(|c| c.to_digit(10).unwrap())
            .collect();
        let mut point = whole.len();
        // lb: 453592370000 = 45359237 × 10^4; kg: 10^12.
        let (factor, shift) = match unit {
            Unit::Kg => (1_u64, 12),
            Unit::Lb => (45_359_237_u64, 4),
        };
        let mut carry = 0_u64;
        for digit in digits.iter_mut().rev() {
            let product = u64::from(*digit) * factor + carry;
            *digit = u32::try_from(product % 10).unwrap();
            carry = product / 10;
        }
        while carry > 0 {
            digits.insert(0, u32::try_from(carry % 10).unwrap());
            carry /= 10;
            point += 1;
        }
        point += shift;
        let integer = digits[..point]
            .iter()
            .fold(0_u64, |acc, d| acc * 10 + u64::from(*d));
        if digits[point] >= 5 {
            integer + 1
        } else {
            integer
        }
    }

    #[test]
    fn display_honours_width_fill_and_alignment() {
        let w = kg(20.0).display_in(Unit::Kg);
        assert_eq!(format!("{w:>8}|"), "   20 kg|");
        assert_eq!(format!("{w:<8}|"), "20 kg   |");
        assert_eq!(format!("{w:8}|"), "20 kg   |");
        assert_eq!(format!("{w:^9}|"), "  20 kg  |");
        assert_eq!(format!("{w:^8}|"), " 20 kg  |");
        assert_eq!(format!("{w:*>8}"), "***20 kg");
        assert_eq!(format!("{w:3}"), "20 kg");
        // Precision stays the number of decimals and does not truncate.
        assert_eq!(
            format!("{:>10.1}|", kg(20.45).display_in(Unit::Kg)),
            "   20.5 kg|"
        );
    }

    #[test]
    fn format_value_just_below_a_half_step_rounds_down() {
        assert_eq!(
            Weight::from_nanograms(499_999_999_999)
                .unwrap()
                .format_value(Unit::Kg, 0),
            "0"
        );
        assert_eq!(
            Weight::from_nanograms(500_000_000_000)
                .unwrap()
                .format_value(Unit::Kg, 0),
            "1"
        );
    }

    fn any_weight() -> impl Strategy<Value = Weight> {
        (0..=MAX_NANOGRAMS).prop_map(Weight)
    }

    proptest! {
        #[test]
        fn serde_round_trips_exactly(weight in any_weight()) {
            let json = serde_json::to_string(&weight).unwrap();
            prop_assert_eq!(serde_json::from_str::<Weight>(&json).unwrap(), weight);
        }

        #[test]
        fn new_matches_the_exact_decimal_oracle(
            fraction in 0.0..=1.0_f64,
            unit in prop_oneof![Just(Unit::Kg), Just(Unit::Lb)],
        ) {
            // Uniform over [0, MAX] in the unit, so most values are long binary fractions.
            let value = fraction * Weight::MAX.value_in(unit);
            if let Ok(weight) = Weight::new(value, unit) {
                prop_assert_eq!(weight.as_nanograms(), reference_nanograms(value, unit));
            }
        }

        #[test]
        fn new_matches_the_oracle_for_arbitrary_bits(bits in 0..0x40A0_0000_0000_0000_u64) {
            // Every non-negative f64 below 2048, including subnormals.
            let value = f64::from_bits(bits);
            match Weight::from_kg(value) {
                Ok(weight) => prop_assert_eq!(weight.as_nanograms(), reference_nanograms(value, Unit::Kg)),
                Err(_) => prop_assert!(reference_nanograms(value, Unit::Kg) > MAX_NANOGRAMS),
            }
        }

        #[test]
        fn kg_to_lb_to_kg_round_trips_exactly(weight in any_weight()) {
            prop_assert_eq!(Weight::from_lb(weight.as_lb()).unwrap(), weight);
            prop_assert_eq!(Weight::from_kg(weight.as_kg()).unwrap(), weight);
        }

        #[test]
        fn lb_with_four_decimals_is_stored_exactly(ten_thousandths in 0..=44_092_452_u64) {
            #[allow(clippy::cast_precision_loss)]
            let value = ten_thousandths as f64 / 10_000.0;
            let weight = Weight::from_lb(value).unwrap();
            prop_assert_eq!(weight.as_nanograms(), ten_thousandths * (NG_PER_LB / 10_000));
            prop_assert_eq!(weight.as_lb(), value);
        }

        #[test]
        fn kg_with_six_decimals_is_stored_exactly(millionths in 0..=2_000_000_000_u64) {
            #[allow(clippy::cast_precision_loss)]
            let value = millionths as f64 / 1_000_000.0;
            let weight = Weight::from_kg(value).unwrap();
            prop_assert_eq!(weight.as_nanograms(), millionths * 1_000_000);
            prop_assert_eq!(weight.as_kg(), value);
        }

        #[test]
        fn round_to_gives_the_expected_multiple(
            weight in any_weight(),
            step in 1..=100_000_000_000_000_u64,
        ) {
            let increment = Weight(step);
            let down = weight.round_to(increment, Rounding::Down).unwrap();
            prop_assert_eq!(down.0 % step, 0);
            prop_assert!(down <= weight && weight.0 - down.0 < step);
            if let Ok(up) = weight.round_to(increment, Rounding::Up) {
                prop_assert_eq!(up.0 % step, 0);
                prop_assert!(up >= weight && up.0 - weight.0 < step);
            }
            if let Ok(nearest) = weight.round_to(increment, Rounding::Nearest) {
                prop_assert!(nearest == down || nearest.0 == down.0 + step);
                prop_assert!(nearest.abs_diff(weight).0 * 2 <= step);
            }
        }

        #[test]
        fn format_value_parses_back_within_half_a_step(weight in any_weight(), decimals in 0..=4_u8) {
            for unit in Unit::ALL {
                let text = weight.format_value(unit, decimals);
                let parsed: f64 = text.parse().unwrap();
                let half_step = 0.5 / 10_f64.powi(i32::from(decimals));
                prop_assert!((parsed - weight.value_in(unit)).abs() <= half_step + 1e-9);
            }
        }
    }
}
