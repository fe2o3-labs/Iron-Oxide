//! The lifter's settings that the engine needs.

use serde::{Deserialize, Serialize};

use crate::{Unit, ValueError, Weight};

/// The unit the lifter loads in, and the step the engine rounds weights to.
///
/// The step is the smallest change the lifter can load: 2.5 kg (two 1.25 kg plates) or 5 lb (two
/// 2.5 lb plates) by default. It is never zero. The unit decides whether a weight written in the
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
    /// [`ValueError::ZeroIncrement`] when `step` is zero.
    pub const fn new(unit: Unit, step: Weight) -> Result<Self, ValueError> {
        if step.is_zero() {
            Err(ValueError::ZeroIncrement)
        } else {
            Ok(Self { unit, step })
        }
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
