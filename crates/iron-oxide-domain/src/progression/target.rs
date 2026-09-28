//! The engine's output: the sets to prefill for the next session.

use serde::{Deserialize, Serialize};

use super::change::ProgressionChange;
use super::engine::SessionVerdict;
use crate::program::RepRange;
use crate::{ExerciseId, Reps, Seconds, Weight};

/// What a set asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetGoal {
    /// A number of reps: `reps` is the value to prefill; `range` is the program's range, if it
    /// has one, for the UI to show.
    Reps {
        /// The reps to aim for.
        reps: Reps,
        /// The program's rep range, or `None` for a fixed count and for warm-ups.
        range: Option<RepRange>,
    },
    /// A timed hold.
    Hold {
        /// Target length of the hold.
        seconds: Seconds,
    },
    /// Work/rest intervals.
    Intervals {
        /// Seconds of work per round.
        work: Seconds,
        /// Seconds of rest between rounds.
        rest: Seconds,
        /// Number of rounds.
        rounds: u16,
    },
}

/// One set to prefill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SetTarget {
    /// The load, or `None` for body-weight work.
    pub weight: Option<Weight>,
    /// Reps or time.
    pub goal: SetGoal,
}

/// Where the working sets of [`ExerciseTargets`] come from. See the prefill order in the
/// [module documentation](super).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    /// The exercise's progression rule applied to its history.
    Progression,
    /// A copy of the last session, for exercises without a rule.
    LastPerformance,
    /// The program's own load and reps: there is no history yet.
    ProgramDefault,
}

/// The next session of one exercise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExerciseTargets {
    /// The exercise.
    pub exercise: ExerciseId,
    /// Where the working sets come from.
    pub source: TargetSource,
    /// Warm-up sets, lightest first, one entry per set.
    pub warmup: Vec<SetTarget>,
    /// Working sets, one entry per set (one entry for intervals).
    pub working: Vec<SetTarget>,
    /// For a load that is a percentage of the training max: the effective training max, after the
    /// history was replayed (for the `training_max` rule) or as given (otherwise).
    pub training_max: Option<Weight>,
    /// Consecutive failed sessions since the last success, hold or deload. The next deload comes
    /// when this reaches the rule's `failures`.
    pub failed_sessions: u16,
    /// How the last session was judged, when the exercise has a rule and a history.
    pub last_verdict: Option<SessionVerdict>,
    /// What the last session changed, for the end-of-session summary, when the exercise has a rule
    /// and a history.
    pub change: Option<ProgressionChange>,
}

/// The result of [`next_targets`](super::next_targets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextTargets {
    /// The targets are known.
    Ready(ExerciseTargets),
    /// The load is a percentage of the training max, and no training max was given: ask the
    /// lifter for it.
    NeedsTrainingMax {
        /// The exercise that needs a training max.
        exercise: ExerciseId,
    },
}

impl NextTargets {
    /// The targets, unless a training max is needed.
    #[must_use]
    pub const fn ready(&self) -> Option<&ExerciseTargets> {
        match self {
            Self::Ready(targets) => Some(targets),
            Self::NeedsTrainingMax { .. } => None,
        }
    }

    /// The exercise these targets are for.
    #[must_use]
    pub const fn exercise(&self) -> &ExerciseId {
        match self {
            Self::Ready(targets) => &targets.exercise,
            Self::NeedsTrainingMax { exercise } => exercise,
        }
    }
}
