//! Properties of the progression engine over random exercises and histories.

// Test helpers outside `#[test]` functions are not covered by clippy.toml's test allowances.
#![allow(clippy::unwrap_used, clippy::panic)]

use iron_oxide_domain::program::{
    Deload, Exercise, Load, ProgressionRule, RepRange, RepTarget, UnitWeight, WarmupLoad,
    WarmupSet, Work,
};
use iron_oxide_domain::progression::{
    ChangeKind, NextTargets, PastSession, ProgressionSettings, SetGoal, TargetSource, WorkingSet,
    next_targets,
};
use iron_oxide_domain::{ExerciseId, Percent, Reps, Rounding, Seconds, Unit, Weight};
use proptest::prelude::*;

fn unit() -> impl Strategy<Value = Unit> {
    prop_oneof![Just(Unit::Kg), Just(Unit::Lb)]
}

/// Any weight, from zero to the cap, biased towards the cap now and then.
fn weight() -> impl Strategy<Value = Weight> {
    let max = Weight::MAX.as_nanograms();
    prop_oneof![
        8 => (0_u32..=400_000).prop_map(|g| Weight::from_kg(f64::from(g) / 1_000.0).unwrap()),
        1 => (max - 10_000_000_000_000..=max).prop_map(|ng| Weight::from_nanograms(ng).unwrap()),
        1 => (0..=max).prop_map(|ng| Weight::from_nanograms(ng).unwrap()),
    ]
}

fn positive_weight() -> impl Strategy<Value = Weight> {
    weight().prop_filter("positive", |w| !w.is_zero())
}

fn percent_up_to(max_basis_points: u32) -> impl Strategy<Value = Percent> {
    (1..=max_basis_points).prop_map(|bp| Percent::from_basis_points(bp).unwrap())
}

fn deload() -> impl Strategy<Value = Option<Deload>> {
    proptest::option::of(
        (1_u16..=10, percent_up_to(5_000))
            .prop_map(|(failures, percent)| Deload { failures, percent }),
    )
}

fn rep_target() -> impl Strategy<Value = RepTarget> {
    prop_oneof![
        (1_u16..=20).prop_map(|reps| RepTarget::Fixed(Reps::new(reps))),
        (1_u16..=15, 0_u16..=10).prop_map(|(min, spread)| RepTarget::Range(RepRange {
            min: Reps::new(min),
            max: Reps::new(min + spread),
        })),
    ]
}

/// An exercise with a weight rule or the training max rule, possibly with warm-ups.
fn exercise() -> impl Strategy<Value = Exercise> {
    (
        1_u16..=6,
        rep_target(),
        positive_weight(),
        unit(),
        positive_weight(),
        deload(),
        0_u8..3,
        percent_up_to(20_000),
        proptest::collection::vec(percent_up_to(9_999), 0..3),
    )
        .prop_map(
            |(sets, target, load, unit, increment, deload, rule, percent, warmups)| {
                let increment = UnitWeight::from_weight(increment, unit);
                let (load, progression, target) = match rule {
                    0 => (
                        Load::Weight(UnitWeight::from_weight(load, unit)),
                        ProgressionRule::AddWhenTopOfRange {
                            increment,
                            deload_after_failures: deload,
                        },
                        target,
                    ),
                    1 => (
                        Load::Weight(UnitWeight::from_weight(load, unit)),
                        ProgressionRule::DoubleProgression {
                            increment,
                            deload_after_failures: deload,
                        },
                        RepTarget::Range(RepRange {
                            min: target.min(),
                            max: target.max(),
                        }),
                    ),
                    _ => (
                        Load::PercentOfTrainingMax(percent),
                        ProgressionRule::TrainingMax {
                            increment,
                            deload_after_failures: deload,
                        },
                        target,
                    ),
                };
                Exercise {
                    id: ExerciseId::new("lift").unwrap(),
                    name: "Lift".to_owned(),
                    work: Work::Reps { sets, reps: target },
                    load: Some(load),
                    rest: Seconds::new(90),
                    tempo: None,
                    notes: None,
                    demo_url: None,
                    warmup: warmups
                        .into_iter()
                        .map(|percent| WarmupSet {
                            sets: 1,
                            reps: Reps::new(5),
                            load: WarmupLoad::PercentOfWorkingWeight(percent),
                        })
                        .collect(),
                    superset: None,
                    progression,
                }
            },
        )
}

fn history() -> impl Strategy<Value = Vec<PastSession>> {
    let set = (proptest::option::of(weight()), 0_u16..=25).prop_map(|(weight, reps)| WorkingSet {
        reps: Reps::new(reps),
        weight,
        duration: None,
    });
    proptest::collection::vec(
        proptest::collection::vec(set, 0..8).prop_map(PastSession::new),
        0..12,
    )
}

fn settings() -> impl Strategy<Value = ProgressionSettings> {
    prop_oneof![
        unit().prop_map(ProgressionSettings::for_unit),
        (unit(), positive_weight())
            .prop_map(|(unit, step)| ProgressionSettings::new(unit, step).unwrap()),
    ]
}

fn on_step(weight: Weight, step: Weight) -> bool {
    weight.round_to(step, Rounding::Down) == Ok(weight)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn targets_are_consistent(
        exercise in exercise(),
        training_max in weight(),
        settings in settings(),
        history in history(),
    ) {
        let outcome = next_targets(&exercise, Some(training_max), settings, &history);
        // Deterministic.
        prop_assert_eq!(&outcome, &next_targets(&exercise, Some(training_max), settings, &history));
        let NextTargets::Ready(targets) = outcome else {
            return Err(TestCaseError::fail("a training max was given"));
        };
        let has_history = history.iter().any(|session| !session.is_empty());
        let is_training_max = exercise.progression.name() == "training_max";
        prop_assert_eq!(targets.change.is_some(), has_history);
        prop_assert_eq!(targets.last_verdict.is_some(), has_history);
        prop_assert_eq!(
            targets.source,
            if has_history { TargetSource::Progression } else { TargetSource::ProgramDefault }
        );
        prop_assert_eq!(targets.training_max.is_some(), is_training_max);

        // One weight and one rep target for every prescribed set, within the range.
        let Work::Reps { sets, reps: target } = exercise.work else { unreachable!() };
        prop_assert_eq!(targets.working.len(), usize::from(sets));
        let first = targets.working[0];
        prop_assert!(targets.working.iter().all(|set| *set == first));
        let Some(working) = first.weight else {
            return Err(TestCaseError::fail("weight rules always have a weight"));
        };
        prop_assert!(working <= Weight::MAX);
        let SetGoal::Reps { reps, .. } = first.goal else { unreachable!() };
        prop_assert!(reps >= target.min() && reps <= target.max());

        // Warm-ups never reach the working weight.
        prop_assert_eq!(targets.warmup.len(), exercise.warmup.len());
        for warmup in &targets.warmup {
            let weight = warmup.weight.unwrap();
            prop_assert!(weight <= working);
            prop_assert!(weight < working || working.as_nanograms() < 10_000);
        }

        // The failure count stays below the deload threshold.
        if let Some(deload) = exercise.progression.deload() {
            prop_assert!(targets.failed_sessions < deload.failures);
        }

        let step = settings.step();
        match targets.change.as_ref().map(|change| change.kind) {
            Some(ChangeKind::WeightIncrease { from, to }
                | ChangeKind::WeightIncreaseRepsReset { from, to, .. }) => {
                prop_assert!(to > from);
                prop_assert_eq!(to, working);
                // On the step, unless the cap left no step above.
                prop_assert!(on_step(to, step) || to.checked_add(step).is_err());
            }
            Some(ChangeKind::Deload { from, to }) => {
                prop_assert!(to <= from);
                prop_assert_eq!(to, working);
                prop_assert!(on_step(to, step) || to < step);
            }
            Some(ChangeKind::TrainingMaxIncrease { from, to }) => {
                prop_assert!(to > from);
                prop_assert_eq!(Some(to), targets.training_max);
            }
            Some(ChangeKind::TrainingMaxDeload { from, to }) => {
                prop_assert!(to <= from);
                prop_assert_eq!(Some(to), targets.training_max);
            }
            Some(ChangeKind::Unchanged { weight, .. }) => prop_assert_eq!(weight, working),
            Some(ChangeKind::RepsIncrease { weight, from, to }) => {
                prop_assert_eq!(weight, working);
                prop_assert!(to > from);
            }
            Some(ChangeKind::TrainingMaxUnchanged { training_max, .. }) => {
                prop_assert_eq!(Some(training_max), targets.training_max);
            }
            None => {}
        }
        // A weight computed from the training max is on the step, or below one step.
        if is_training_max {
            prop_assert!(on_step(working, step) || working < step || working == Weight::MAX);
        }
    }

    /// Appending a failed session never increases the working weight or the training max.
    #[test]
    fn a_failure_never_increases(
        exercise in exercise(),
        training_max in weight(),
        settings in settings(),
        history in history(),
        weight in weight(),
    ) {
        let before = next_targets(&exercise, Some(training_max), settings, &history);
        let NextTargets::Ready(before) = before else { unreachable!() };
        let target = before.working[0].weight.unwrap();
        // Every set lifted at the target (or the given weight for weight rules) with 0 reps.
        let lifted = if exercise.progression.name() == "training_max" { target } else { weight };
        let mut longer = history.clone();
        longer.push(PastSession::new(vec![WorkingSet::new(lifted, Reps::ZERO); 3]));
        let NextTargets::Ready(after) = next_targets(&exercise, Some(training_max), settings, &longer)
        else { unreachable!() };
        prop_assert!(after.working[0].weight.unwrap() <= lifted.max(target));
        if let (Some(tm_before), Some(tm_after)) = (before.training_max, after.training_max) {
            prop_assert!(tm_after <= tm_before);
        }
    }
}
