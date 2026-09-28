//! What a session changed for next time, for the end-of-session summary.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{ExerciseId, Reps, Unit, Weight};

/// Decimals shown in change descriptions: enough for 1.25 kg plates.
const DECIMALS: u8 = 2;

/// How an exercise's target moved after a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The working weight goes up: `Squat: 100 → 102.5 kg`.
    WeightIncrease {
        /// The weight lifted.
        from: Weight,
        /// The next target.
        to: Weight,
    },
    /// Double progression reached the top of the range: the weight goes up and the reps go back
    /// to the bottom: `Row: 50 → 52.5 kg, reps 12 → 8`.
    WeightIncreaseRepsReset {
        /// The weight lifted.
        from: Weight,
        /// The next target.
        to: Weight,
        /// The reps reached on every set (the top of the range).
        reps_from: Reps,
        /// The next rep target (the bottom of the range).
        reps_to: Reps,
    },
    /// Double progression within the range: same weight, one more rep: `Row: reps 8 → 9 at 50 kg`.
    RepsIncrease {
        /// The weight, unchanged.
        weight: Weight,
        /// The reps reached on every set.
        from: Reps,
        /// The next rep target.
        to: Reps,
    },
    /// A failure streak cut the working weight: `Bench: deload 80 → 72.5 kg`.
    Deload {
        /// The weight lifted.
        from: Weight,
        /// The next target.
        to: Weight,
    },
    /// The training max goes up: `Bench: training max 100 → 102.5 kg`.
    TrainingMaxIncrease {
        /// The training max before.
        from: Weight,
        /// The training max after.
        to: Weight,
    },
    /// A failure streak cut the training max: `Bench: deload, training max 100 → 90 kg`.
    TrainingMaxDeload {
        /// The training max before.
        from: Weight,
        /// The training max after.
        to: Weight,
    },
    /// The working weight stays: `Squat: stays at 100 kg (1 failed session)`.
    Unchanged {
        /// The weight, unchanged.
        weight: Weight,
        /// Consecutive failed sessions so far (0 after a hold or a success at the cap).
        failed_sessions: u16,
    },
    /// The training max stays: `Bench: training max stays at 100 kg`.
    TrainingMaxUnchanged {
        /// The training max, unchanged.
        training_max: Weight,
        /// Consecutive failed sessions so far.
        failed_sessions: u16,
    },
}

impl ChangeKind {
    /// Whether the target (weight, reps or training max) moves up.
    #[must_use]
    pub const fn is_progress(self) -> bool {
        matches!(
            self,
            Self::WeightIncrease { .. }
                | Self::WeightIncreaseRepsReset { .. }
                | Self::RepsIncrease { .. }
                | Self::TrainingMaxIncrease { .. }
        )
    }

    /// Whether this is a deload.
    #[must_use]
    pub const fn is_deload(self) -> bool {
        matches!(self, Self::Deload { .. } | Self::TrainingMaxDeload { .. })
    }
}

/// What the last session changed for one exercise.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProgressionChange {
    /// The exercise.
    pub exercise: ExerciseId,
    /// The exercise's name, as shown in the summary.
    pub name: String,
    /// The change.
    pub kind: ChangeKind,
}

impl ProgressionChange {
    /// A one-line description in `unit`: `Squat: 100 → 102.5 kg`.
    #[must_use]
    pub const fn display_in(&self, unit: Unit) -> ProgressionChangeDisplay<'_> {
        ProgressionChangeDisplay { change: self, unit }
    }
}

/// A [`ProgressionChange`] described in a unit. Built by [`ProgressionChange::display_in`].
#[derive(Debug, Clone, Copy)]
pub struct ProgressionChangeDisplay<'a> {
    change: &'a ProgressionChange,
    unit: Unit,
}

impl fmt::Display for ProgressionChangeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = self.unit;
        let number = |weight: Weight| weight.format_value(unit, DECIMALS);
        let with_unit = |weight: Weight| format!("{} {unit}", number(weight));
        write!(f, "{}: ", self.change.name)?;
        match self.change.kind {
            ChangeKind::WeightIncrease { from, to } => {
                write!(f, "{} → {}", number(from), with_unit(to))
            }
            ChangeKind::WeightIncreaseRepsReset {
                from,
                to,
                reps_from,
                reps_to,
            } => write!(
                f,
                "{} → {}, reps {reps_from} → {reps_to}",
                number(from),
                with_unit(to)
            ),
            ChangeKind::RepsIncrease { weight, from, to } => {
                write!(f, "reps {from} → {to} at {}", with_unit(weight))
            }
            ChangeKind::Deload { from, to } => {
                write!(f, "deload {} → {}", number(from), with_unit(to))
            }
            ChangeKind::TrainingMaxIncrease { from, to } => {
                write!(f, "training max {} → {}", number(from), with_unit(to))
            }
            ChangeKind::TrainingMaxDeload { from, to } => {
                write!(
                    f,
                    "deload, training max {} → {}",
                    number(from),
                    with_unit(to)
                )
            }
            ChangeKind::Unchanged {
                weight,
                failed_sessions,
            } => {
                write!(f, "stays at {}", with_unit(weight))?;
                failures(f, failed_sessions)
            }
            ChangeKind::TrainingMaxUnchanged {
                training_max,
                failed_sessions,
            } => {
                write!(f, "training max stays at {}", with_unit(training_max))?;
                failures(f, failed_sessions)
            }
        }
    }
}

fn failures(f: &mut fmt::Formatter<'_>, count: u16) -> fmt::Result {
    match count {
        0 => Ok(()),
        1 => f.write_str(" (1 failed session)"),
        _ => write!(f, " ({count} failed sessions)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn change(name: &str, kind: ChangeKind) -> ProgressionChange {
        ProgressionChange {
            exercise: ExerciseId::new(name.to_lowercase()).unwrap(),
            name: name.to_owned(),
            kind,
        }
    }

    #[test]
    fn descriptions() {
        let cases = [
            (
                change(
                    "Squat",
                    ChangeKind::WeightIncrease {
                        from: kg(100.0),
                        to: kg(102.5),
                    },
                ),
                "Squat: 100 → 102.5 kg",
            ),
            (
                change(
                    "Row",
                    ChangeKind::WeightIncreaseRepsReset {
                        from: kg(50.0),
                        to: kg(52.5),
                        reps_from: Reps::new(12),
                        reps_to: Reps::new(8),
                    },
                ),
                "Row: 50 → 52.5 kg, reps 12 → 8",
            ),
            (
                change(
                    "Row",
                    ChangeKind::RepsIncrease {
                        weight: kg(50.0),
                        from: Reps::new(8),
                        to: Reps::new(9),
                    },
                ),
                "Row: reps 8 → 9 at 50 kg",
            ),
            (
                change(
                    "Bench",
                    ChangeKind::Deload {
                        from: kg(80.0),
                        to: kg(72.5),
                    },
                ),
                "Bench: deload 80 → 72.5 kg",
            ),
            (
                change(
                    "Bench",
                    ChangeKind::TrainingMaxIncrease {
                        from: kg(100.0),
                        to: kg(102.5),
                    },
                ),
                "Bench: training max 100 → 102.5 kg",
            ),
            (
                change(
                    "Bench",
                    ChangeKind::TrainingMaxDeload {
                        from: kg(100.0),
                        to: kg(90.0),
                    },
                ),
                "Bench: deload, training max 100 → 90 kg",
            ),
            (
                change(
                    "Squat",
                    ChangeKind::Unchanged {
                        weight: kg(100.0),
                        failed_sessions: 0,
                    },
                ),
                "Squat: stays at 100 kg",
            ),
            (
                change(
                    "Squat",
                    ChangeKind::Unchanged {
                        weight: kg(100.0),
                        failed_sessions: 1,
                    },
                ),
                "Squat: stays at 100 kg (1 failed session)",
            ),
            (
                change(
                    "Bench",
                    ChangeKind::TrainingMaxUnchanged {
                        training_max: kg(100.0),
                        failed_sessions: 2,
                    },
                ),
                "Bench: training max stays at 100 kg (2 failed sessions)",
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(change.display_in(Unit::Kg).to_string(), expected);
        }
    }

    #[test]
    fn description_in_pounds() {
        let lb = |value| Weight::from_lb(value).unwrap();
        let squat = change(
            "Squat",
            ChangeKind::WeightIncrease {
                from: lb(225.0),
                to: lb(230.0),
            },
        );
        assert_eq!(
            squat.display_in(Unit::Lb).to_string(),
            "Squat: 225 → 230 lb"
        );
    }

    #[test]
    fn classification() {
        let up = ChangeKind::RepsIncrease {
            weight: kg(50.0),
            from: Reps::new(8),
            to: Reps::new(9),
        };
        assert!(up.is_progress() && !up.is_deload());
        let down = ChangeKind::TrainingMaxDeload {
            from: kg(100.0),
            to: kg(90.0),
        };
        assert!(down.is_deload() && !down.is_progress());
        let same = ChangeKind::Unchanged {
            weight: kg(100.0),
            failed_sessions: 0,
        };
        assert!(!same.is_deload() && !same.is_progress());
    }
}
