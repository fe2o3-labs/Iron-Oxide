//! The rules: judging past sessions and computing the next targets.

use serde::{Deserialize, Serialize};

use super::change::{ChangeKind, ProgressionChange};
use super::history::{PastSession, WorkingSet};
use super::settings::ProgressionSettings;
use super::target::{ExerciseTargets, NextTargets, SetGoal, SetTarget, TargetSource};
use crate::program::{Deload, Exercise, Load, ProgressionRule, RepTarget, WarmupLoad, Work};
use crate::{Percent, Reps, Rounding, Weight};

/// How a past session went against the program's rep target. See the
/// [module documentation](super) for the exact definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionVerdict {
    /// Every prescribed set reached the top of the target.
    Success,
    /// Every prescribed set is within the rep range, but not all at the top.
    Hold,
    /// At least one prescribed set is missing or below the target.
    Failure,
}

/// The next session of `exercise`: the sets to prefill, and what the last session in `history`
/// changed.
///
/// - `training_max`: the training max the lifter last entered for this exercise, needed when its
///   load is a percentage of the training max.
/// - `history`: this exercise's past sessions, **oldest first**, filtered as described in the
///   [module documentation](super) (build it with [`exercise_history`](super::exercise_history)).
///
/// Returns [`NextTargets::NeedsTrainingMax`] when the load is a percentage of the training max
/// and `training_max` is `None`. Never fails otherwise: a program that did not pass validation
/// (e.g. a rule on timed work) is handled as if it had no rule.
#[must_use]
pub fn next_targets(
    exercise: &Exercise,
    training_max: Option<Weight>,
    settings: ProgressionSettings,
    history: &[PastSession],
) -> NextTargets {
    let percent_of_training_max = match exercise.load {
        Some(Load::PercentOfTrainingMax(percent)) => Some(percent),
        Some(Load::Weight(_)) | None => None,
    };
    let training_max = match (percent_of_training_max, training_max) {
        (Some(percent), Some(training_max)) => Some((percent, training_max)),
        (Some(_), None) => {
            return NextTargets::NeedsTrainingMax {
                exercise: exercise.id.clone(),
            };
        }
        (None, _) => None,
    };
    let step = settings.step();
    let sessions: Vec<&PastSession> = history.iter().filter(|s| !s.is_empty()).collect();
    let default_weight = match exercise.load {
        Some(Load::Weight(weight)) => Some(weight.weight()),
        Some(Load::PercentOfTrainingMax(_)) => {
            training_max.map(|(percent, training_max)| percent_weight(training_max, percent, step))
        }
        None => None,
    };

    let plan = match (exercise.work, exercise.progression) {
        (
            Work::Reps { sets, reps },
            ProgressionRule::AddWhenTopOfRange {
                increment,
                deload_after_failures,
            },
        ) if !sessions.is_empty() => weight_rule(
            &sessions,
            &WeightRule {
                sets,
                target: reps,
                increment: increment.weight(),
                deload: deload_after_failures,
                double: false,
                step,
            },
        ),
        (
            Work::Reps { sets, reps },
            ProgressionRule::DoubleProgression {
                increment,
                deload_after_failures,
            },
        ) if !sessions.is_empty() => weight_rule(
            &sessions,
            &WeightRule {
                sets,
                target: reps,
                increment: increment.weight(),
                deload: deload_after_failures,
                double: true,
                step,
            },
        ),
        (
            Work::Reps { sets, reps },
            ProgressionRule::TrainingMax {
                increment,
                deload_after_failures,
            },
        ) => match training_max {
            Some((percent, training_max)) => training_max_rule(
                &sessions,
                &TrainingMaxRule {
                    sets,
                    target: reps,
                    percent,
                    training_max,
                    increment: increment.weight(),
                    deload: deload_after_failures,
                    step,
                },
            ),
            None => no_rule(exercise, &sessions, default_weight),
        },
        _ => no_rule(exercise, &sessions, default_weight),
    };

    let working_weight = plan.working.iter().filter_map(|set| set.weight).max();
    NextTargets::Ready(ExerciseTargets {
        exercise: exercise.id.clone(),
        source: plan.source,
        warmup: warmup(exercise, working_weight, step),
        working: plan.working,
        training_max: plan
            .training_max
            .or(training_max.map(|(_, training_max)| training_max)),
        failed_sessions: plan.failed_sessions,
        last_verdict: plan.last_verdict,
        change: plan.change.map(|kind| ProgressionChange {
            exercise: exercise.id.clone(),
            name: exercise.name.clone(),
            kind,
        }),
    })
}

/// The working sets and state computed by one rule.
struct Plan {
    source: TargetSource,
    working: Vec<SetTarget>,
    training_max: Option<Weight>,
    failed_sessions: u16,
    last_verdict: Option<SessionVerdict>,
    change: Option<ChangeKind>,
}

/// Judges one session: the `n`-th best rep count among the counted sets decides. Sets lighter
/// than `at_least` do not count. Also returns that rep count.
fn judge(
    sets: &[WorkingSet],
    prescribed: u16,
    target: RepTarget,
    at_least: Option<Weight>,
) -> (SessionVerdict, Reps) {
    let mut reps: Vec<Reps> = sets
        .iter()
        .filter(|set| at_least.is_none_or(|minimum| weight_of(set) >= minimum))
        .map(|set| set.reps)
        .collect();
    reps.sort_unstable_by(|a, b| b.cmp(a));
    let n = usize::from(prescribed.max(1));
    let Some(&reached) = reps.get(n - 1) else {
        return (SessionVerdict::Failure, Reps::ZERO);
    };
    let verdict = if reached.is_zero() || reached < target.min() {
        SessionVerdict::Failure
    } else if reached >= target.max() {
        SessionVerdict::Success
    } else {
        SessionVerdict::Hold
    };
    (verdict, reached)
}

/// A set's load, zero for body-weight work.
fn weight_of(set: &WorkingSet) -> Weight {
    set.weight.unwrap_or(Weight::ZERO)
}

/// Counts a failure, and says whether it triggers a deload.
fn count_failure(failures: &mut u16, deload: Option<Deload>) -> Option<Percent> {
    *failures = failures.saturating_add(1);
    match deload {
        Some(deload) if *failures >= deload.failures.max(1) => {
            *failures = 0;
            Some(deload.percent)
        }
        _ => None,
    }
}

fn reps_goal(target: RepTarget, aim: Reps) -> SetGoal {
    SetGoal::Reps {
        reps: aim,
        range: match target {
            RepTarget::Fixed(_) => None,
            RepTarget::Range(range) => Some(range),
        },
    }
}

fn uniform_sets(sets: u16, weight: Option<Weight>, goal: SetGoal) -> Vec<SetTarget> {
    vec![SetTarget { weight, goal }; usize::from(sets)]
}

struct WeightRule {
    sets: u16,
    target: RepTarget,
    increment: Weight,
    deload: Option<Deload>,
    double: bool,
    step: Weight,
}

/// `add_when_top_of_range` and `double_progression`, from a non-empty history.
fn weight_rule(sessions: &[&PastSession], rule: &WeightRule) -> Plan {
    let (min, max) = (rule.target.min(), rule.target.max());
    let mut failures = 0;
    let mut weight = Weight::ZERO;
    let mut aim = if rule.double { min } else { max };
    let mut last_verdict = None;
    let mut change = None;
    for session in sessions {
        let base = session
            .sets
            .iter()
            .map(weight_of)
            .min()
            .unwrap_or(Weight::ZERO);
        let (verdict, reached) = judge(&session.sets, rule.sets, rule.target, None);
        weight = base;
        let unchanged = |failed_sessions| ChangeKind::Unchanged {
            weight: base,
            failed_sessions,
        };
        let kind = match verdict {
            SessionVerdict::Success => {
                failures = 0;
                let to = increase(base, rule.increment, rule.step);
                if to > base {
                    weight = to;
                    if rule.double {
                        aim = min;
                        ChangeKind::WeightIncreaseRepsReset {
                            from: base,
                            to,
                            reps_from: reached,
                            reps_to: min,
                        }
                    } else {
                        ChangeKind::WeightIncrease { from: base, to }
                    }
                } else {
                    // At the cap: nothing heavier to load.
                    aim = max;
                    unchanged(0)
                }
            }
            SessionVerdict::Hold => {
                failures = 0;
                if rule.double {
                    // reached < max here, so one more rep stays within the range.
                    aim = Reps::new(reached.get().saturating_add(1)).min(max);
                    ChangeKind::RepsIncrease {
                        weight: base,
                        from: reached,
                        to: aim,
                    }
                } else {
                    unchanged(0)
                }
            }
            SessionVerdict::Failure => {
                aim = if rule.double { min } else { max };
                match count_failure(&mut failures, rule.deload) {
                    Some(percent) => {
                        weight = deload_weight(base, percent, rule.step);
                        ChangeKind::Deload {
                            from: base,
                            to: weight,
                        }
                    }
                    None => unchanged(failures),
                }
            }
        };
        last_verdict = Some(verdict);
        change = Some(kind);
    }
    Plan {
        source: TargetSource::Progression,
        working: uniform_sets(rule.sets, Some(weight), reps_goal(rule.target, aim)),
        training_max: None,
        failed_sessions: failures,
        last_verdict,
        change,
    }
}

struct TrainingMaxRule {
    sets: u16,
    target: RepTarget,
    percent: Percent,
    training_max: Weight,
    increment: Weight,
    deload: Option<Deload>,
    step: Weight,
}

/// `training_max`: replays the history on the training max, starting from the one entered.
fn training_max_rule(sessions: &[&PastSession], rule: &TrainingMaxRule) -> Plan {
    let mut training_max = rule.training_max;
    let mut failures = 0;
    let mut last_verdict = None;
    let mut change = None;
    for session in sessions {
        let target_weight = percent_weight(training_max, rule.percent, rule.step);
        let (verdict, _) = judge(&session.sets, rule.sets, rule.target, Some(target_weight));
        let before = training_max;
        let unchanged = |failed_sessions| ChangeKind::TrainingMaxUnchanged {
            training_max: before,
            failed_sessions,
        };
        let kind = match verdict {
            SessionVerdict::Success => {
                failures = 0;
                training_max = before.checked_add(rule.increment).unwrap_or(Weight::MAX);
                if training_max > before {
                    ChangeKind::TrainingMaxIncrease {
                        from: before,
                        to: training_max,
                    }
                } else {
                    unchanged(0)
                }
            }
            SessionVerdict::Hold => {
                failures = 0;
                unchanged(0)
            }
            SessionVerdict::Failure => match count_failure(&mut failures, rule.deload) {
                Some(percent) => {
                    training_max = deload_weight(before, percent, rule.step);
                    ChangeKind::TrainingMaxDeload {
                        from: before,
                        to: training_max,
                    }
                }
                None => unchanged(failures),
            },
        };
        last_verdict = Some(verdict);
        change = Some(kind);
    }
    let weight = percent_weight(training_max, rule.percent, rule.step);
    Plan {
        source: if sessions.is_empty() {
            TargetSource::ProgramDefault
        } else {
            TargetSource::Progression
        },
        working: uniform_sets(
            rule.sets,
            Some(weight),
            reps_goal(rule.target, rule.target.max()),
        ),
        training_max: Some(training_max),
        failed_sessions: failures,
        last_verdict,
        change,
    }
}

/// No rule (or timed work): the last performance, else the program default.
fn no_rule(exercise: &Exercise, sessions: &[&PastSession], default_weight: Option<Weight>) -> Plan {
    let (count, goal) = match exercise.work {
        Work::Reps { sets, reps } => {
            let aim = match (reps, exercise.progression) {
                (RepTarget::Range(range), ProgressionRule::DoubleProgression { .. }) => range.min,
                _ => reps.max(),
            };
            (sets, reps_goal(reps, aim))
        }
        Work::Hold { sets, seconds } => (sets, SetGoal::Hold { seconds }),
        Work::Intervals { work, rest, rounds } => (1, SetGoal::Intervals { work, rest, rounds }),
    };
    let Some(last) = sessions.last() else {
        return Plan {
            source: TargetSource::ProgramDefault,
            working: uniform_sets(count, default_weight, goal),
            training_max: None,
            failed_sessions: 0,
            last_verdict: None,
            change: None,
        };
    };
    let working = (0..usize::from(count))
        .map(|index| {
            let done = last.sets.get(index).or_else(|| last.sets.last());
            let weight = done.and_then(|set| set.weight).or(default_weight);
            let goal = match (goal, exercise.work, done) {
                (
                    SetGoal::Reps { range, .. },
                    Work::Reps {
                        reps: RepTarget::Range(bounds),
                        ..
                    },
                    Some(set),
                ) => SetGoal::Reps {
                    reps: set.reps.clamp(bounds.min, bounds.max.max(bounds.min)),
                    range,
                },
                _ => goal,
            };
            SetTarget { weight, goal }
        })
        .collect();
    Plan {
        source: TargetSource::LastPerformance,
        working,
        training_max: None,
        failed_sessions: 0,
        last_verdict: None,
        change: None,
    }
}

/// The warm-up sets, from the heaviest working weight.
fn warmup(exercise: &Exercise, working: Option<Weight>, step: Weight) -> Vec<SetTarget> {
    exercise
        .warmup
        .iter()
        .flat_map(|line| {
            let weight = match line.load {
                WarmupLoad::Weight(weight) => Some(weight.weight()),
                WarmupLoad::PercentOfWorkingWeight(percent) => {
                    working.map(|working| lighter_share(working, percent, step))
                }
            };
            let target = SetTarget {
                weight,
                goal: SetGoal::Reps {
                    reps: line.reps,
                    range: None,
                },
            };
            std::iter::repeat_n(target, usize::from(line.sets))
        })
        .collect()
}

/// `exact` rounded to `step`, falling back to rounding down if rounding up passes
/// [`Weight::MAX`], and to `exact` itself if the rounding gives zero.
fn round_or_exact(exact: Weight, step: Weight, rounding: Rounding) -> Weight {
    let rounded = exact
        .round_to(step, rounding)
        .or_else(|_| exact.round_to(step, Rounding::Down))
        .unwrap_or(exact);
    if rounded.is_zero() { exact } else { rounded }
}

/// `base + increment`, rounded to the nearest step, or up when the nearest does not move it (an
/// increment smaller than half a step), or down when rounding up passes [`Weight::MAX`], or the
/// exact sum as a last resort. Returns `base` when nothing heavier can be loaded (at
/// [`Weight::MAX`], or a zero increment).
fn increase(base: Weight, increment: Weight, step: Weight) -> Weight {
    if increment.is_zero() {
        return base;
    }
    let raw = base.checked_add(increment).unwrap_or(Weight::MAX);
    [Rounding::Nearest, Rounding::Up, Rounding::Down]
        .into_iter()
        .filter_map(|rounding| raw.round_to(step, rounding).ok())
        .chain([raw])
        .find(|&weight| weight > base)
        .unwrap_or(base)
}

/// `weight × (1 − percent)`, rounded like a warm-up: see [`lighter_share`].
fn deload_weight(weight: Weight, percent: Percent, step: Weight) -> Weight {
    let kept = Percent::HUNDRED
        .basis_points()
        .saturating_sub(percent.basis_points());
    // `kept` is at most 100 %, so the conversion cannot fail.
    let kept = Percent::from_basis_points(kept).unwrap_or(Percent::HUNDRED);
    lighter_share(weight, kept, step)
}

/// `percent` of the training max, rounded to the nearest step.
fn percent_weight(training_max: Weight, percent: Percent, step: Weight) -> Weight {
    let exact = percent.of(training_max).unwrap_or(Weight::MAX);
    round_or_exact(exact, step, Rounding::Nearest)
}

/// `percent` of `weight` (a warm-up share, or what a deload keeps), rounded to the nearest step,
/// or down when the nearest would not be lighter than `weight`. Never heavier than `weight`.
fn lighter_share(weight: Weight, percent: Percent, step: Weight) -> Weight {
    let exact = percent.of(weight).unwrap_or(weight);
    [Rounding::Nearest, Rounding::Down]
        .into_iter()
        .map(|rounding| round_or_exact(exact, step, rounding))
        .find(|&share| share < weight)
        .unwrap_or_else(|| exact.min(weight))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Unit;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    fn pct(value: f64) -> Percent {
        Percent::new(value).unwrap()
    }

    fn sets(weight: f64, reps: &[u16]) -> Vec<WorkingSet> {
        reps.iter()
            .map(|&r| WorkingSet::new(kg(weight), Reps::new(r)))
            .collect()
    }

    const STEP_KG: f64 = 2.5;

    #[test]
    fn judging_fixed_reps() {
        let five = RepTarget::Fixed(Reps::new(5));
        let judge = |reps: &[u16]| judge(&sets(100.0, reps), 3, five, None).0;
        assert_eq!(judge(&[5, 5, 5]), SessionVerdict::Success);
        assert_eq!(judge(&[6, 5, 7]), SessionVerdict::Success);
        assert_eq!(judge(&[5, 5, 4]), SessionVerdict::Failure);
        assert_eq!(judge(&[5, 5]), SessionVerdict::Failure, "missing set");
        assert_eq!(judge(&[5, 5, 0]), SessionVerdict::Failure, "failed attempt");
        assert_eq!(judge(&[5, 3, 5, 5]), SessionVerdict::Success, "extra set");
        assert_eq!(judge(&[]), SessionVerdict::Failure);
    }

    #[test]
    fn judging_ranges() {
        let range = RepTarget::Range(crate::program::RepRange {
            min: Reps::new(8),
            max: Reps::new(12),
        });
        let judge = |reps: &[u16]| judge(&sets(50.0, reps), 3, range, None);
        assert_eq!(
            judge(&[12, 12, 12]),
            (SessionVerdict::Success, Reps::new(12))
        );
        assert_eq!(judge(&[12, 10, 9]), (SessionVerdict::Hold, Reps::new(9)));
        assert_eq!(judge(&[8, 8, 8]), (SessionVerdict::Hold, Reps::new(8)));
        assert_eq!(judge(&[12, 12, 7]), (SessionVerdict::Failure, Reps::new(7)));
    }

    #[test]
    fn judging_ignores_sets_lighter_than_the_target() {
        let five = RepTarget::Fixed(Reps::new(5));
        let mut done = sets(100.0, &[5, 5]);
        done.push(WorkingSet::new(kg(90.0), Reps::new(5)));
        assert_eq!(
            judge(&done, 3, five, Some(kg(100.0))).0,
            SessionVerdict::Failure
        );
        assert_eq!(
            judge(&done, 3, five, Some(kg(90.0))).0,
            SessionVerdict::Success
        );
        let bodyweight = [WorkingSet::bodyweight(Reps::new(5)); 3];
        assert_eq!(
            judge(&bodyweight, 3, five, Some(Weight::ZERO)).0,
            SessionVerdict::Success
        );
    }

    #[test]
    fn increase_rounds_to_the_step() {
        let step = kg(STEP_KG);
        assert_eq!(increase(kg(100.0), kg(2.5), step), kg(102.5));
        assert_eq!(increase(kg(100.0), kg(5.0), step), kg(105.0));
        // Smaller than the step: goes up to the next step instead of stalling.
        assert_eq!(increase(kg(100.0), kg(1.0), step), kg(102.5));
        // Off the grid: nearest step above.
        assert_eq!(increase(kg(101.0), kg(2.5), step), kg(102.5));
        assert_eq!(increase(kg(101.0), kg(1.0), step), kg(102.5));
        // Pounds.
        let step = lb(5.0);
        assert_eq!(increase(lb(225.0), lb(5.0), step), lb(230.0));
        assert_eq!(increase(lb(225.0), lb(2.5), step), lb(230.0));
        // A kg increment on a lb grid.
        assert_eq!(increase(lb(225.0), kg(2.5), step), lb(230.0));
        // Zero increment and the cap.
        assert_eq!(increase(kg(100.0), Weight::ZERO, kg(STEP_KG)), kg(100.0));
        assert_eq!(increase(Weight::MAX, kg(2.5), kg(STEP_KG)), Weight::MAX);
        let top = Weight::MAX.round_to(step, Rounding::Down).unwrap();
        assert_eq!(
            increase(top, lb(5.0), step),
            Weight::MAX,
            "the exact sum, capped"
        );
        assert_eq!(increase(Weight::MAX, lb(5.0), step), Weight::MAX);
        // From zero (a body-weight log), a small increment goes to the first step.
        assert_eq!(increase(Weight::ZERO, kg(0.5), kg(STEP_KG)), kg(2.5));
    }

    #[test]
    fn deload_rounds_to_a_lighter_step() {
        let step = kg(STEP_KG);
        assert_eq!(deload_weight(kg(100.0), pct(10.0), step), kg(90.0));
        assert_eq!(deload_weight(kg(80.0), pct(10.0), step), kg(72.5));
        assert_eq!(deload_weight(kg(82.5), pct(10.0), step), kg(75.0));
        // Off the grid, a small cut: the nearest step is heavier, so round down.
        assert_eq!(deload_weight(kg(24.0), pct(1.0), step), kg(22.5));
        // Tiny weights keep the exact value rather than zero.
        assert_eq!(deload_weight(kg(2.0), pct(10.0), step), kg(1.8));
        assert_eq!(deload_weight(kg(100.0), Percent::ZERO, step), kg(100.0));
        assert_eq!(
            deload_weight(kg(100.0), Percent::HUNDRED, step),
            Weight::ZERO
        );
        assert_eq!(deload_weight(kg(100.0), Percent::MAX, step), Weight::ZERO);
        assert_eq!(deload_weight(Weight::ZERO, pct(10.0), step), Weight::ZERO);
        assert_eq!(deload_weight(lb(135.0), pct(10.0), lb(5.0)), lb(120.0));
        assert_eq!(deload_weight(lb(225.0), pct(10.0), lb(5.0)), lb(205.0));
    }

    #[test]
    fn percentages_round_to_the_nearest_step() {
        let step = kg(STEP_KG);
        assert_eq!(percent_weight(kg(100.0), pct(77.5), step), kg(77.5));
        assert_eq!(percent_weight(kg(103.0), pct(75.0), step), kg(77.5));
        assert_eq!(percent_weight(kg(20.0), pct(1.0), step), kg(0.2));
        assert_eq!(
            percent_weight(Weight::MAX, Percent::MAX, step),
            Weight::MAX,
            "saturates"
        );
        assert_eq!(percent_weight(lb(200.0), pct(80.0), lb(5.0)), lb(160.0));
        assert_eq!(percent_weight(lb(210.0), pct(75.0), lb(5.0)), lb(160.0));
    }

    #[test]
    fn warmups_stay_lighter_than_the_working_weight() {
        let step = kg(STEP_KG);
        assert_eq!(lighter_share(kg(100.0), pct(50.0), step), kg(50.0));
        assert_eq!(lighter_share(kg(102.5), pct(60.0), step), kg(62.5));
        // Nearest would reach the working weight: rounded down instead.
        assert_eq!(lighter_share(kg(20.0), pct(95.0), step), kg(17.5));
        // Down would be zero: the exact value.
        assert_eq!(lighter_share(kg(2.5), pct(90.0), step), kg(2.25));
        assert_eq!(lighter_share(kg(2.5), pct(40.0), step), kg(1.0));
        assert_eq!(lighter_share(Weight::ZERO, pct(50.0), step), Weight::ZERO);
        assert_eq!(lighter_share(lb(135.0), pct(50.0), lb(5.0)), lb(70.0));
    }

    #[test]
    fn rounding_falls_back_near_the_cap() {
        let step = ProgressionSettings::for_unit(Unit::Lb).step();
        let rounded = round_or_exact(Weight::MAX, step, Rounding::Up);
        assert!(rounded <= Weight::MAX && rounded > Weight::ZERO);
        assert_eq!(rounded.round_to(step, Rounding::Down).unwrap(), rounded);
    }

    #[test]
    fn failures_count_up_to_the_deload() {
        let deload = Some(Deload {
            failures: 2,
            percent: pct(10.0),
        });
        let mut failures = 0;
        assert_eq!(count_failure(&mut failures, deload), None);
        assert_eq!(failures, 1);
        assert_eq!(count_failure(&mut failures, deload), Some(pct(10.0)));
        assert_eq!(failures, 0);
        assert_eq!(count_failure(&mut failures, None), None);
        assert_eq!(failures, 1);
        // A (non-validated) zero means every failure deloads.
        let zero = Some(Deload {
            failures: 0,
            percent: pct(10.0),
        });
        assert_eq!(count_failure(&mut 0, zero), Some(pct(10.0)));
        let mut many = u16::MAX;
        assert_eq!(count_failure(&mut many, None), None);
        assert_eq!(many, u16::MAX);
    }
}
