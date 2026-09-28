//! The lifter's settings that the engine needs.

use serde::{Deserialize, Serialize};

use crate::{Quantity, Unit, ValueError, Weight};

/// [`ProgressionSettings::max_step`] in nanograms: 2.5 kg.
const MAX_STEP_NANOGRAMS: u64 = 2_500_000_000_000;

/// The unit the lifter loads in, and the step the engine rounds weights to.
///
/// The step is the smallest change the lifter can load: 2.5 kg (two 1.25 kg plates) or 5 lb (two
/// 2.5 lb plates) by default. It is never zero and never above [`max_step`](Self::max_step). The unit decides whether a weight written in the
/// program must be converted: a program load written in the other unit is rounded to the step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ProgressionSettingsRepr")]
pub struct ProgressionSettings {
    unit: Unit,
    step: Weight,
}

/// The unchecked shape of stored [`ProgressionSettings`].
#[derive(Deserialize)]
struct ProgressionSettingsRepr {
    unit: Unit,
    step: Weight,
}

impl TryFrom<ProgressionSettingsRepr> for ProgressionSettings {
    type Error = ValueError;

    fn try_from(repr: ProgressionSettingsRepr) -> Result<Self, Self::Error> {
        Self::new(repr.unit, repr.step)
    }
}

impl ProgressionSettings {
    /// Loads in `unit` and rounds to `step`.
    ///
    /// # Errors
    /// [`ValueError::ZeroIncrement`] when `step` is zero, and [`ValueError::TooLarge`] when it is
    /// above [`max_step`](Self::max_step).
    pub const fn new(unit: Unit, step: Weight) -> Result<Self, ValueError> {
        if step.is_zero() {
            Err(ValueError::ZeroIncrement)
        } else if step.as_nanograms() > MAX_STEP_NANOGRAMS {
            Err(ValueError::TooLarge {
                quantity: Quantity::Weight,
                max: "2.5 kg (5.51 lb)",
            })
        } else {
            Ok(Self { unit, step })
        }
    }

    /// The largest step: 2.5 kg (5.51 lb), which covers both defaults (5 lb is 2.27 kg).
    ///
    /// Any step from 1 ng up to this one is accepted, in either unit: the engine only needs the
    /// bound. Offering steps the lifter's plates can make (1.25 kg, 2.5 kg, 2.5 lb, 5 lb…) is the
    /// settings screen's job; an odd step such as 5.5 lb just rounds targets to multiples of it.
    ///
    /// The bound lets past training max sessions be judged without the current settings: any
    /// target rounded to the nearest step is within half of it of the exact percentage, so a
    /// fixed tolerance of half this step (see the [module documentation](super)) accepts every
    /// target the app could have shown, whatever the settings were then or are now.
    #[must_use]
    pub fn max_step() -> Weight {
        Weight::from_nanograms(MAX_STEP_NANOGRAMS).unwrap_or(Weight::MAX)
    }

    /// The default for a lifter who loads in `unit`: a step of 2.5 kg or 5 lb.
    #[must_use]
    pub fn for_unit(unit: Unit) -> Self {
        let step = match unit {
            // 2.5 kg and 5 lb in nanograms: exact.
            Unit::Kg => 2_500_000_000_000,
            Unit::Lb => 2_267_961_850_000,
        };
        match Weight::from_nanograms(step) {
            Ok(step) => Self { unit, step },
            // Unreachable: both values are far below Weight::MAX.
            Err(_) => Self {
                unit,
                step: Weight::MAX,
            },
        }
    }

    /// The unit the lifter loads in.
    #[must_use]
    pub const fn unit(self) -> Unit {
        self.unit
    }

    /// The loadable step computed weights are rounded to.
    #[must_use]
    pub const fn step(self) -> Weight {
        self.step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_per_unit() {
        let kg = ProgressionSettings::for_unit(Unit::Kg);
        assert_eq!(kg.unit(), Unit::Kg);
        assert_eq!(kg.step(), Weight::from_kg(2.5).unwrap());
        let lb = ProgressionSettings::for_unit(Unit::Lb);
        assert_eq!(lb.unit(), Unit::Lb);
        assert_eq!(lb.step(), Weight::from_lb(5.0).unwrap());
    }

    #[test]
    fn custom_step() {
        let step = Weight::from_kg(1.0).unwrap();
        let settings = ProgressionSettings::new(Unit::Kg, step).unwrap();
        assert_eq!(settings.step(), step);
        assert_eq!(settings.unit(), Unit::Kg);
        assert_eq!(
            ProgressionSettings::new(Unit::Lb, Weight::ZERO),
            Err(ValueError::ZeroIncrement)
        );
        let max = ProgressionSettings::max_step();
        assert_eq!(max, Weight::from_kg(2.5).unwrap());
        assert_eq!(ProgressionSettings::new(Unit::Kg, max).unwrap().step(), max);
        let above = Weight::from_nanograms(max.as_nanograms() + 1).unwrap();
        assert!(matches!(
            ProgressionSettings::new(Unit::Kg, above),
            Err(ValueError::TooLarge { .. })
        ));
        assert!(ProgressionSettings::for_unit(Unit::Lb).step() <= max);
    }

    #[test]
    fn serde_round_trip_and_rejects_zero() {
        let settings = ProgressionSettings::for_unit(Unit::Kg);
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(json, r#"{"unit":"kg","step":2.5}"#);
        assert_eq!(
            serde_json::from_str::<ProgressionSettings>(&json).unwrap(),
            settings
        );
        let error =
            serde_json::from_str::<ProgressionSettings>(r#"{"unit":"lb","step":0}"#).unwrap_err();
        assert!(error.to_string().contains("greater than zero"), "{error}");
    }
}
