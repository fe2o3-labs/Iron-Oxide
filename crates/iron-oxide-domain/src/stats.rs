//! Strength statistics: estimated one-rep max, training volume, top sets, personal records and the
//! per-exercise series behind the history charts.
//!
//! Every function here takes plain [`PerformedSet`] values, so it does not depend on how sessions
//! are stored. A logged set maps into a [`PerformedSet`] by copying its weight, reps and warm-up
//! flag.
//!
//! # Which sets count
//!
//! - Warm-up sets never count: not for volume, top sets, e1RM or records.
//! - A working set with 0 reps (a failed attempt) adds nothing to volume and is not a top set or a
//!   record: a weight counts once it has been lifted at least once.

mod e1rm;
mod records;
mod series;
mod volume;

pub use e1rm::{E1rmFormula, MAX_E1RM_REPS, estimate_1rm};
pub use records::{ExerciseRecords, PrEvent, PrKind, detect_prs};
pub use series::{SeriesPoint, exercise_series};
pub use volume::{Volume, VolumeDisplay, session_volume};

use serde::{Deserialize, Serialize};

use crate::{Reps, Weight};

/// One set as it was performed: the input of every statistic in this module.
///
/// This is deliberately minimal so that any stored set (with IDs, timestamps, notes, ...) can be
/// turned into one by copying three fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PerformedSet {
    /// The load.
    pub weight: Weight,
    /// Repetitions done; 0 for a failed attempt.
    pub reps: Reps,
    /// Whether this was a warm-up set. Warm-ups are left out of every statistic.
    pub warmup: bool,
}

impl PerformedSet {
    /// A working set.
    #[must_use]
    pub const fn working(weight: Weight, reps: Reps) -> Self {
        Self {
            weight,
            reps,
            warmup: false,
        }
    }

    /// A warm-up set.
    #[must_use]
    pub const fn warmup(weight: Weight, reps: Reps) -> Self {
        Self {
            weight,
            reps,
            warmup: true,
        }
    }

    /// Whether this set counts towards top sets, e1RM and records: a working set with at least one
    /// rep.
    #[must_use]
    pub const fn counts(self) -> bool {
        !self.warmup && !self.reps.is_zero()
    }

    /// Weight × reps for a working set, zero for a warm-up.
    #[must_use]
    pub fn volume(self) -> Volume {
        if self.warmup {
            Volume::ZERO
        } else {
            Volume::of(self.weight, self.reps)
        }
    }

    /// The estimated one-rep max of this set, or `None` for a warm-up or when the formula gives no
    /// estimate (see [`E1rmFormula::estimate`]).
    #[must_use]
    pub fn e1rm(self, formula: E1rmFormula) -> Option<Weight> {
        if self.warmup {
            None
        } else {
            formula.estimate(self.weight, self.reps)
        }
    }

    const fn lift(self) -> Lift {
        Lift {
            weight: self.weight,
            reps: self.reps,
        }
    }
}

/// A working set that was completed: a weight lifted for a number of reps (at least one).
///
/// Used for top sets, record events and chart points. Lifts order by weight, then by reps, so the
/// maximum of a collection is its top set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Lift {
    /// The load. Declared first so that the derived order compares it first.
    pub weight: Weight,
    /// Repetitions done.
    pub reps: Reps,
}

/// The top set of one exercise in one session: the heaviest working set, and on a tie in weight the
/// one with more reps. `None` when no working set has at least one rep.
///
/// Pass the sets of a single exercise.
pub fn top_set(sets: impl IntoIterator<Item = PerformedSet>) -> Option<Lift> {
    sets.into_iter()
        .filter(|set| set.counts())
        .map(PerformedSet::lift)
        .max()
}

/// The best estimated one-rep max among the sets, with the set that produced it. On equal
/// estimates the heavier set wins. `None` when no set has an estimate.
pub(crate) fn best_e1rm(
    sets: impl IntoIterator<Item = PerformedSet>,
    formula: E1rmFormula,
) -> Option<(Weight, Lift)> {
    sets.into_iter()
        .filter_map(|set| set.e1rm(formula).map(|e1rm| (e1rm, set.lift())))
        .max()
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::PerformedSet;
    use crate::{Reps, Weight};

    pub fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    pub fn reps(count: u16) -> Reps {
        Reps::new(count)
    }

    pub fn work(weight: f64, count: u16) -> PerformedSet {
        PerformedSet::working(kg(weight), reps(count))
    }

    pub fn warm(weight: f64, count: u16) -> PerformedSet {
        PerformedSet::warmup(kg(weight), reps(count))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{kg, reps, warm, work};
    use super::*;

    fn lift(weight: f64, count: u16) -> Lift {
        Lift {
            weight: kg(weight),
            reps: reps(count),
        }
    }

    #[test]
    fn constructors_set_the_warmup_flag() {
        assert!(!PerformedSet::working(kg(100.0), reps(5)).warmup);
        assert!(PerformedSet::warmup(kg(60.0), reps(5)).warmup);
    }

    #[test]
    fn counts_only_working_sets_with_reps() {
        assert!(work(100.0, 1).counts());
        assert!(!work(100.0, 0).counts());
        assert!(!warm(100.0, 5).counts());
        assert!(!warm(100.0, 0).counts());
    }

    #[test]
    fn set_volume_excludes_warmups() {
        assert_eq!(work(100.0, 5).volume(), Volume::of(kg(100.0), reps(5)));
        assert_eq!(warm(100.0, 5).volume(), Volume::ZERO);
        assert_eq!(work(100.0, 0).volume(), Volume::ZERO);
    }

    #[test]
    fn set_e1rm_excludes_warmups() {
        assert_eq!(work(100.0, 1).e1rm(E1rmFormula::Epley), Some(kg(100.0)));
        assert_eq!(warm(100.0, 1).e1rm(E1rmFormula::Epley), None);
        assert_eq!(work(100.0, 0).e1rm(E1rmFormula::Epley), None);
    }

    #[test]
    fn top_set_is_the_heaviest_then_the_most_reps() {
        let sets = [
            warm(140.0, 1),
            work(100.0, 8),
            work(120.0, 3),
            work(120.0, 5),
            work(120.0, 4),
            work(130.0, 0),
        ];
        assert_eq!(top_set(sets), Some(lift(120.0, 5)));
    }

    #[test]
    fn top_set_is_none_without_a_completed_working_set() {
        assert_eq!(top_set([]), None);
        assert_eq!(top_set([warm(60.0, 5), warm(80.0, 3)]), None);
        assert_eq!(top_set([work(100.0, 0)]), None);
    }

    #[test]
    fn top_set_with_identical_sets() {
        assert_eq!(
            top_set([work(100.0, 5), work(100.0, 5)]),
            Some(lift(100.0, 5))
        );
    }

    #[test]
    fn top_set_of_bodyweight_sets() {
        assert_eq!(top_set([work(0.0, 10), work(0.0, 12)]), Some(lift(0.0, 12)));
    }

    #[test]
    fn best_e1rm_picks_the_highest_estimate() {
        // Epley: 100 × 5 → 116.67, 110 × 2 → 117.33, 90 × 10 → 120.
        let sets = [
            work(100.0, 5),
            work(110.0, 2),
            work(90.0, 10),
            warm(200.0, 1),
        ];
        assert_eq!(
            best_e1rm(sets, E1rmFormula::Epley),
            Some((kg(120.0), lift(90.0, 10)))
        );
    }

    #[test]
    fn best_e1rm_prefers_the_heavier_set_on_a_tie() {
        // Epley: 120 × 1 → 120 and 90 × 10 → 120.
        let sets = [work(90.0, 10), work(120.0, 1)];
        assert_eq!(
            best_e1rm(sets, E1rmFormula::Epley),
            Some((kg(120.0), lift(120.0, 1)))
        );
    }

    #[test]
    fn best_e1rm_is_none_without_an_estimate() {
        assert_eq!(best_e1rm([], E1rmFormula::Epley), None);
        assert_eq!(
            best_e1rm([work(60.0, 20), warm(100.0, 1)], E1rmFormula::Epley),
            None
        );
    }
}
