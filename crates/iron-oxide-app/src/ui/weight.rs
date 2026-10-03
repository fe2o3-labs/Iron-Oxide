//! Showing weights in the user's unit (#26).
//!
//! Weights stay the domain's exact [`Weight`] everywhere; the unit (kg or lb, from the user's
//! settings) only matters when one is shown. These helpers are the one place that decides how:
//! up to two decimals (enough for 1.25 kg and 1.25 lb plates), rounded half-up, no trailing zeros.
//! The arithmetic is the domain's integer arithmetic, never floats.

use dioxus::prelude::*;
use iron_oxide_domain::{Rounding, Unit, Weight};

/// Decimals shown for a weight.
pub const WEIGHT_DECIMALS: u8 = 2;

/// The number alone, for big numerals: `"100"`, `"102.5"`, `"44.09"`.
#[must_use]
pub fn weight_number(weight: Weight, unit: Unit) -> String {
    weight.format_value(unit, WEIGHT_DECIMALS)
}

/// The number and the unit symbol, for running text: `"102.5 kg"`.
#[must_use]
pub fn weight_text(weight: Weight, unit: Unit) -> String {
    format!("{} {}", weight_number(weight, unit), unit.symbol())
}

/// What an estimate (an e1RM) is rounded to for display: 0.5 kg or 1 lb. An estimate is not a
/// load on the bar, so `169.17 kg` would be false precision.
#[must_use]
pub fn estimate_increment(unit: Unit) -> Weight {
    let nanograms = match unit {
        Unit::Kg => 500_000_000_000,
        Unit::Lb => 453_592_370_000,
    };
    Weight::from_nanograms(nanograms).unwrap_or(Weight::ZERO)
}

/// An estimate rounded to the nearest [`estimate_increment`] of `unit` (exact, integer).
#[must_use]
pub fn rounded_estimate(weight: Weight, unit: Unit) -> Weight {
    weight
        .round_to(estimate_increment(unit), Rounding::Nearest)
        .unwrap_or(weight)
}

/// An estimate's number alone, rounded: `"169"`, `"93.5"`, `"373"`.
#[must_use]
pub fn estimate_number(weight: Weight, unit: Unit) -> String {
    weight_number(rounded_estimate(weight, unit), unit)
}

/// An estimate with its unit symbol, rounded: `"169 kg"`.
#[must_use]
pub fn estimate_text(weight: Weight, unit: Unit) -> String {
    weight_text(rounded_estimate(weight, unit), unit)
}

/// The unit's name for screen readers: `"kilograms"`, `"pounds"`.
#[must_use]
pub const fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Kg => "kilograms",
        Unit::Lb => "pounds",
    }
}

/// Which way a stepper moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Down,
    Up,
}

/// `weight` moved one `step` in `direction`, kept within `min..=max`. Exact: integer nanograms.
#[must_use]
pub fn step_weight(
    weight: Weight,
    step: Weight,
    direction: Direction,
    min: Weight,
    max: Weight,
) -> Weight {
    let moved = match direction {
        Direction::Up => weight.checked_add(step).unwrap_or(Weight::MAX),
        Direction::Down => weight.saturating_sub(step),
    };
    moved.clamp(min, max.max(min))
}

/// The user's display unit, provided by the app root. Kilograms until the settings screen (#34)
/// loads the user's choice into it.
#[derive(Clone, Copy, PartialEq)]
pub struct UnitSetting(pub Signal<Unit>);

/// Provides the unit setting. Called once, by the app root.
pub fn use_unit_provider() -> UnitSetting {
    let unit = use_signal(|| Unit::Kg);
    use_context_provider(|| UnitSetting(unit))
}

/// The user's display unit.
#[must_use]
pub fn use_unit() -> Unit {
    *use_context::<UnitSetting>().0.read()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    #[test]
    fn whole_numbers_have_no_decimals() {
        assert_eq!(weight_number(kg(100.0), Unit::Kg), "100");
        assert_eq!(weight_number(kg(0.0), Unit::Kg), "0");
        assert_eq!(weight_number(lb(45.0), Unit::Lb), "45");
        assert_eq!(weight_number(lb(135.0), Unit::Lb), "135");
    }

    #[test]
    fn decimals_lose_their_trailing_zeros() {
        assert_eq!(weight_number(kg(102.5), Unit::Kg), "102.5");
        assert_eq!(weight_number(kg(1.25), Unit::Kg), "1.25");
        assert_eq!(weight_number(kg(102.50), Unit::Kg), "102.5");
        assert_eq!(weight_number(lb(2.5), Unit::Lb), "2.5");
    }

    #[test]
    fn values_round_half_up_to_two_decimals() {
        // 20 kg = 44.0924… lb; 100 kg = 220.4622… lb.
        assert_eq!(weight_number(kg(20.0), Unit::Lb), "44.09");
        assert_eq!(weight_number(kg(100.0), Unit::Lb), "220.46");
        // 45 lb = 20.41165… kg.
        assert_eq!(weight_number(lb(45.0), Unit::Kg), "20.41");
        // Exact halfway values round up.
        assert_eq!(weight_number(kg(0.125), Unit::Kg), "0.13");
        assert_eq!(weight_number(kg(1.005), Unit::Kg), "1.01");
        // Rounding can carry into the whole part, which then has no decimals.
        assert_eq!(weight_number(kg(99.996), Unit::Kg), "100");
    }

    #[test]
    fn text_has_the_unit_symbol() {
        assert_eq!(weight_text(kg(102.5), Unit::Kg), "102.5 kg");
        assert_eq!(weight_text(lb(225.0), Unit::Lb), "225 lb");
        assert_eq!(unit_name(Unit::Kg), "kilograms");
        assert_eq!(unit_name(Unit::Lb), "pounds");
    }

    #[test]
    fn estimates_round_to_half_a_kilo_or_a_pound() {
        assert_eq!(estimate_text(kg(169.17), Unit::Kg), "169 kg");
        assert_eq!(estimate_text(kg(93.33), Unit::Kg), "93.5 kg");
        assert_eq!(estimate_text(kg(116.25), Unit::Kg), "116.5 kg");
        assert_eq!(estimate_number(kg(100.0), Unit::Kg), "100");
        // 169.17 kg = 372.95 lb.
        assert_eq!(estimate_text(kg(169.17), Unit::Lb), "373 lb");
        assert_eq!(estimate_number(lb(225.4), Unit::Lb), "225");
        assert_eq!(rounded_estimate(Weight::ZERO, Unit::Kg), Weight::ZERO);
        // Set weights keep their exact display.
        assert_eq!(weight_text(kg(102.25), Unit::Kg), "102.25 kg");
    }

    #[test]
    fn steps_are_exact_and_stay_in_range() {
        let step = kg(2.5);
        let (min, max) = (Weight::ZERO, kg(500.0));
        assert_eq!(
            step_weight(kg(100.0), step, Direction::Up, min, max),
            kg(102.5)
        );
        assert_eq!(
            step_weight(kg(100.0), step, Direction::Down, min, max),
            kg(97.5)
        );
        assert_eq!(
            step_weight(kg(1.0), step, Direction::Down, min, max),
            Weight::ZERO
        );
        assert_eq!(step_weight(kg(499.0), step, Direction::Up, min, max), max);
        assert_eq!(
            step_weight(Weight::MAX, step, Direction::Up, min, Weight::MAX),
            Weight::MAX
        );
        // Pound steps add up exactly: 45 + 4 × 2.5 lb is 55 lb, not 54.999….
        let mut weight = lb(45.0);
        for _ in 0..4 {
            weight = step_weight(weight, lb(2.5), Direction::Up, min, Weight::MAX);
        }
        assert_eq!(weight, lb(55.0));
        assert_eq!(weight_number(weight, Unit::Lb), "55");
        // A bar never goes below its own weight.
        assert_eq!(
            step_weight(kg(20.0), step, Direction::Down, kg(20.0), max),
            kg(20.0)
        );
    }
}
