//! The progression engine: the targets of an exercise's next session, and what changed.
//!
//! [`next_targets`] takes one exercise of a program, the lifter's training max (if any), the
//! [`ProgressionSettings`] and the exercise's recent history ([`PastSession`]s, oldest first), and
//! returns [`NextTargets`]: the working and warm-up sets to prefill, where they come from
//! ([`TargetSource`]), and the [`ProgressionChange`] that the last session caused, for the
//! end-of-session summary ("Squat: 100 → 102.5 kg"). It is pure and deterministic: no clock, no
//! storage, and the same input always gives the same output.
//!
//! # History
//!
//! The caller passes the history of **one exercise**, oldest session first. Build it with
//! [`exercise_history`] from the stored [`SessionLog`]s, after filtering them:
//!
//! - Only sessions of the **active program** (every version of it, no other program), as for
//!   [`next_day`]. Progression does not carry over from one program to another: another program
//!   may use the same exercise with a different rep scheme (5 × 5 against 3 × 8–12), so its loads
//!   say little about this one. A new program starts from its own loads.
//! - For an exercise loaded as a percentage of its training max, only sessions **completed after
//!   the training max was last entered** (see [Training max](#training-max)).
//!
//! [`exercise_history`] keeps completed sessions only (abandoned ones would count as failures),
//! and only their working sets of the exercise, in set order. Sessions where the exercise has no
//! working set (it was skipped) are ignored by the engine: they are neither a success nor a failure.
//!
//! # Judging a session
//!
//! Each past session gets a [`SessionVerdict`]. With `n` the prescribed number of working sets and
//! the program's rep target (a fixed count, where the bottom and top are the same, or a range):
//!
//! - **Success**: at least `n` working sets reached the **top** of the target (the fixed count, or
//!   the top of the range).
//! - **Hold** (ranges only): at least `n` working sets reached the **bottom** of the range, but
//!   fewer than `n` reached the top. Not a failure: it neither progresses the weight nor counts
//!   towards a deload, and it ends a failure streak.
//! - **Failure**: fewer than `n` working sets reached the bottom of the target. A missing set, a
//!   set with fewer reps than the target and a failed attempt (0 reps) all count as missed sets.
//!
//! Extra sets beyond `n` are allowed and never hurt: the best `n` sets are judged. For the
//! training max rule, only sets at least as heavy as that session's target weight count, so a
//! session done lighter than prescribed cannot raise the training max.
//!
//! # Rules
//!
//! Every computed weight is rounded to the settings' loadable [step](ProgressionSettings::step)
//! (2.5 kg or 5 lb by default); see [Rounding](#rounding).
//!
//! - **None** (and every timed exercise, which cannot have a rule): no progression. The targets
//!   are the last performance, else the program default.
//! - **`add_when_top_of_range`**: the base weight is the weight lifted last session, the lightest
//!   of its working sets. After a success, the next weight is base + increment; after a hold or a
//!   failure, it stays the base. Reps aim for the top of the target.
//! - **`double_progression`**: the weight stays the base while the reps climb. After a hold, the
//!   rep target becomes the lowest reps of the best `n` sets plus one ("reps 8 → 9"). After a
//!   success, the weight goes up by the increment and the rep target goes back to the bottom of
//!   the range. After a failure, the rep target is the bottom of the range.
//! - **`training_max`**: the working weight is the load's percentage of the training max. A
//!   success adds the increment to the training max (exactly, unrounded); a failure streak cuts it.
//! - **`deload_after_failures`** (any rule): after `failures` consecutive failed sessions, the
//!   weight (or the training max) is cut to `× (1 − percent)`, rounded to a lighter step, and the failure
//!   count starts again from zero, so the session after a deload needs a fresh streak before the
//!   next one. A success or a hold also resets the count.
//!
//! The base weight is what was actually lifted, not what was prescribed: going heavier than the
//! target moves the base up, and going lighter moves it down.
//!
//! # Training max
//!
//! Training maxes are kept by the app per user and per exercise, outside the program. The value to
//! pass is the one **the lifter last entered**, together with the history since then. The engine
//! replays that history to get the current, effective training max (returned in
//! [`ExerciseTargets::training_max`]), so calling it twice with the same input never applies an
//! increase twice. The app must not store the effective training max back unless it also starts
//! the history again from that point.
//!
//! A load that is a percentage of the training max, with no training max given, returns
//! [`NextTargets::NeedsTrainingMax`], whatever the rule, so the app can ask for it.
//!
//! # Prefill order
//!
//! The working sets come from the first of these that applies ([`TargetSource`]):
//!
//! 1. **Progression**: the exercise has a rule and a history. One weight and one rep target for
//!    every set.
//! 2. **Last performance**: no rule (or timed work) and a history. Set `i` repeats the weight of
//!    working set `i` of the last session (the last set when fewer were done). Reps stay the
//!    program's fixed count; for a range, the last reps clamped into the range. Timed work keeps
//!    the program's seconds and rounds: a short hold last time does not shorten the target.
//! 3. **Program default**: no history. The program's load (a percentage of the training max
//!    rounded to the step), and the fixed count, or for a range the top of it (the bottom for
//!    double progression).
//!
//! Warm-up sets follow the program's warm-up lines, computed from the heaviest working weight:
//! fixed warm-up weights as written, percentages of the working weight rounded to the step and
//! never as heavy as the working weight.
//!
//! # Rounding
//!
//! Weights taken as they are (the program's fixed load, a weight lifted last session, a fixed
//! warm-up) are never rounded. Computed weights are:
//!
//! - an increase to the nearest step, or the next step up when the nearest would not move it, so
//!   an increment smaller than the step still progresses (100 kg + 1 kg with a 2.5 kg step is
//!   102.5 kg);
//! - a percentage of the training max to the nearest step;
//! - a deload, and a warm-up percentage of the working weight, to the nearest step, or down when
//!   the nearest would not be lighter: a deload never increases the weight, and a warm-up is never
//!   as heavy as the working weight.
//!
//! A computed weight that would round to zero keeps its exact value instead. Nothing goes above
//! [`Weight::MAX`]: an increase stops at the cap (off the step if no step fits below it), and at
//! the cap it leaves the weight where it is.
//!
//! [`SessionLog`]: crate::SessionLog
//! [`next_day`]: crate::next_day
//! [`Weight::MAX`]: crate::Weight::MAX

mod change;
mod engine;
mod history;
mod settings;
mod target;

pub use change::{ChangeKind, ProgressionChange, ProgressionChangeDisplay};
pub use engine::{SessionVerdict, next_targets};
pub use history::{PastSession, WorkingSet, exercise_history};
pub use settings::ProgressionSettings;
pub use target::{ExerciseTargets, NextTargets, SetGoal, SetTarget, TargetSource};
