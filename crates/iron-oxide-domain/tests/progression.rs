//! Scenarios of the progression engine, through its public API.

// Test helpers outside `#[test]` functions are not covered by clippy.toml's test allowances.
#![allow(clippy::unwrap_used, clippy::panic)]

use iron_oxide_domain::program::{
    Deload, Exercise, Load, ProgressionRule, RepRange, RepTarget, UnitWeight, WarmupLoad,
    WarmupSet, Work, builtin_programs,
};
use iron_oxide_domain::progression::{
    ChangeKind, ExerciseTargets, NextTargets, PastSession, ProgressionSettings, SessionVerdict,
    SetGoal, SetTarget, TargetSource, WorkingSet, exercise_history, next_targets,
};
use iron_oxide_domain::{
    DayId, ExerciseId, LoggedSet, Percent, ProgramVersionId, Reps, Seconds, SessionId, SessionLog,
    SetId, Unit, Weight,
};

fn kg(value: f64) -> Weight {
    Weight::from_kg(value).unwrap()
}

fn lb(value: f64) -> Weight {
    Weight::from_lb(value).unwrap()
}

fn pct(value: f64) -> Percent {
    Percent::new(value).unwrap()
}

fn unit_weight(value: f64, unit: Unit) -> UnitWeight {
    UnitWeight::new(value, unit).unwrap()
}

fn kg_settings() -> ProgressionSettings {
    ProgressionSettings::for_unit(Unit::Kg)
}

fn deload(failures: u16, percent: f64) -> Option<Deload> {
    Some(Deload {
        failures,
        percent: pct(percent),
    })
}

fn exercise(name: &str, work: Work, load: Option<Load>, progression: ProgressionRule) -> Exercise {
    Exercise {
        id: ExerciseId::new(name.to_lowercase().replace(' ', "-")).unwrap(),
        name: name.to_owned(),
        work,
        load,
        rest: Seconds::new(120),
        tempo: None,
        notes: None,
        demo_url: None,
        warmup: Vec::new(),
        superset: None,
        progression,
    }
}

fn fixed(sets: u16, reps: u16) -> Work {
    Work::Reps {
        sets,
        reps: RepTarget::Fixed(Reps::new(reps)),
    }
}

fn range(sets: u16, min: u16, max: u16) -> Work {
    Work::Reps {
        sets,
        reps: RepTarget::Range(RepRange {
            min: Reps::new(min),
            max: Reps::new(max),
        }),
    }
}

/// Squat 3 × 5 at 100 kg, +2.5 kg, deload 10 % after 3 failures.
fn squat() -> Exercise {
    exercise(
        "Squat",
        fixed(3, 5),
        Some(Load::Weight(unit_weight(100.0, Unit::Kg))),
        ProgressionRule::AddWhenTopOfRange {
            increment: unit_weight(2.5, Unit::Kg),
            deload_after_failures: deload(3, 10.0),
        },
    )
}

/// Row 3 × 8–12 at 50 kg, double progression +2.5 kg, deload 10 % after 2 failures.
fn row() -> Exercise {
    exercise(
        "Row",
        range(3, 8, 12),
        Some(Load::Weight(unit_weight(50.0, Unit::Kg))),
        ProgressionRule::DoubleProgression {
            increment: unit_weight(2.5, Unit::Kg),
            deload_after_failures: deload(2, 10.0),
        },
    )
}

/// Bench 3 × 5 at 80 % of the training max, +2.5 kg, deload 10 % after 2 failures.
fn bench() -> Exercise {
    exercise(
        "Bench",
        fixed(3, 5),
        Some(Load::PercentOfTrainingMax(pct(80.0))),
        ProgressionRule::TrainingMax {
            increment: unit_weight(2.5, Unit::Kg),
            deload_after_failures: deload(2, 10.0),
        },
    )
}

fn session(weight: Weight, reps: &[u16]) -> PastSession {
    reps.iter()
        .map(|&r| WorkingSet::new(weight, Reps::new(r)))
        .collect()
}

fn ready(outcome: NextTargets) -> ExerciseTargets {
    match outcome {
        NextTargets::Ready(targets) => targets,
        NextTargets::NeedsTrainingMax { exercise } => panic!("{exercise} needs a training max"),
    }
}

fn targets(exercise: &Exercise, history: &[PastSession]) -> ExerciseTargets {
    ready(next_targets(exercise, None, kg_settings(), history))
}

/// The single working weight and reps of uniform targets.
fn working(targets: &ExerciseTargets) -> (Weight, Reps) {
    let first = targets.working[0];
    assert!(
        targets.working.iter().all(|set| *set == first),
        "{targets:?}"
    );
    match first {
        SetTarget {
            weight: Some(weight),
            goal: SetGoal::Reps { reps, .. },
        } => (weight, reps),
        other => panic!("not a weighted reps target: {other:?}"),
    }
}

fn change(targets: &ExerciseTargets) -> ChangeKind {
    targets.change.as_ref().unwrap().kind
}

fn describe(targets: &ExerciseTargets, unit: Unit) -> String {
    targets
        .change
        .as_ref()
        .unwrap()
        .display_in(unit)
        .to_string()
}

mod add_when_top_of_range {
    use super::*;

    #[test]
    fn no_history_is_the_program_default() {
        let next = targets(&squat(), &[]);
        assert_eq!(next.source, TargetSource::ProgramDefault);
        assert_eq!(working(&next), (kg(100.0), Reps::new(5)));
        assert_eq!(next.working.len(), 3);
        assert_eq!(next.change, None);
        assert_eq!(next.last_verdict, None);
        assert_eq!(next.failed_sessions, 0);
        assert_eq!(next.training_max, None);
        // Sessions where the exercise was skipped are no history.
        assert_eq!(targets(&squat(), &[PastSession::default()]), next);
    }

    #[test]
    fn success_adds_the_increment() {
        let next = targets(&squat(), &[session(kg(100.0), &[5, 5, 5])]);
        assert_eq!(next.source, TargetSource::Progression);
        assert_eq!(next.last_verdict, Some(SessionVerdict::Success));
        assert_eq!(working(&next), (kg(102.5), Reps::new(5)));
        assert_eq!(
            change(&next),
            ChangeKind::WeightIncrease {
                from: kg(100.0),
                to: kg(102.5)
            }
        );
        assert_eq!(describe(&next, Unit::Kg), "Squat: 100 → 102.5 kg");
    }

    #[test]
    fn success_streak_builds_on_the_weight_lifted() {
        let history = [
            session(kg(100.0), &[5, 5, 5]),
            session(kg(102.5), &[5, 5, 5]),
            session(kg(105.0), &[5, 5, 5]),
        ];
        assert_eq!(working(&targets(&squat(), &history)).0, kg(107.5));
    }

    #[test]
    fn the_base_is_the_weight_actually_lifted() {
        // Heavier than the program: progression continues from there.
        let heavier = targets(&squat(), &[session(kg(120.0), &[5, 5, 5])]);
        assert_eq!(working(&heavier).0, kg(122.5));
        // A lighter last set: the lightest working set is the base.
        let mut dropped = session(kg(100.0), &[5, 5]);
        dropped.sets.push(WorkingSet::new(kg(90.0), Reps::new(5)));
        assert_eq!(working(&targets(&squat(), &[dropped])).0, kg(92.5));
    }

    #[test]
    fn sets_logged_without_a_weight_do_not_drop_the_base() {
        let mut slip = session(kg(100.0), &[5, 5]);
        slip.sets.push(WorkingSet::bodyweight(Reps::new(5)));
        assert_eq!(working(&targets(&squat(), &[slip])).0, kg(102.5));
        // No weight at all: the program's load is the base.
        let none = PastSession::new(vec![WorkingSet::bodyweight(Reps::new(5)); 3]);
        assert_eq!(working(&targets(&squat(), &[none])).0, kg(102.5));
    }

    #[test]
    fn a_missed_rep_is_a_failure() {
        let next = targets(&squat(), &[session(kg(100.0), &[5, 5, 4])]);
        assert_eq!(next.last_verdict, Some(SessionVerdict::Failure));
        assert_eq!(working(&next), (kg(100.0), Reps::new(5)));
        assert_eq!(next.failed_sessions, 1);
        assert_eq!(
            change(&next),
            ChangeKind::Unchanged {
                weight: kg(100.0),
                failed_sessions: 1
            }
        );
        assert_eq!(
            describe(&next, Unit::Kg),
            "Squat: stays at 100 kg (1 failed session)"
        );
    }

    #[test]
    fn a_missing_set_is_a_failure() {
        let next = targets(&squat(), &[session(kg(100.0), &[5, 5])]);
        assert_eq!(next.last_verdict, Some(SessionVerdict::Failure));
    }

    #[test]
    fn deload_after_the_failure_streak_then_the_count_restarts() {
        let fail = || session(kg(100.0), &[5, 5, 3]);
        let two = targets(&squat(), &[fail(), fail()]);
        assert_eq!(working(&two).0, kg(100.0));
        assert_eq!(two.failed_sessions, 2);

        let three = targets(&squat(), &[fail(), fail(), fail()]);
        assert_eq!(working(&three).0, kg(90.0));
        assert_eq!(three.failed_sessions, 0);
        assert_eq!(
            change(&three),
            ChangeKind::Deload {
                from: kg(100.0),
                to: kg(90.0)
            }
        );
        assert_eq!(describe(&three, Unit::Kg), "Squat: deload 100 → 90 kg");

        // One more failure at the deloaded weight: no second deload.
        let four = targets(
            &squat(),
            &[fail(), fail(), fail(), session(kg(90.0), &[5, 5, 4])],
        );
        assert_eq!(working(&four).0, kg(90.0));
        assert_eq!(four.failed_sessions, 1);
        assert!(!change(&four).is_deload());
    }

    #[test]
    fn a_success_breaks_the_failure_streak() {
        let history = [
            session(kg(100.0), &[5, 5, 3]),
            session(kg(100.0), &[5, 5, 3]),
            session(kg(100.0), &[5, 5, 5]),
            session(kg(102.5), &[5, 5, 3]),
            session(kg(102.5), &[5, 4, 3]),
        ];
        let next = targets(&squat(), &history);
        assert_eq!(next.failed_sessions, 2);
        assert_eq!(working(&next).0, kg(102.5));
    }

    #[test]
    fn skipped_sessions_do_not_break_or_extend_the_streak() {
        let fail = || session(kg(100.0), &[5, 5, 3]);
        let history = [
            fail(),
            PastSession::default(),
            fail(),
            PastSession::default(),
            fail(),
        ];
        let next = targets(&squat(), &history);
        assert!(change(&next).is_deload());
    }

    #[test]
    fn without_deload_failures_accumulate() {
        let mut no_deload = squat();
        no_deload.progression = ProgressionRule::AddWhenTopOfRange {
            increment: unit_weight(2.5, Unit::Kg),
            deload_after_failures: None,
        };
        let history = vec![session(kg(100.0), &[5, 5, 3]); 12];
        let next = targets(&no_deload, &history);
        assert_eq!(working(&next).0, kg(100.0));
        assert_eq!(next.failed_sessions, 12);
    }

    #[test]
    fn with_a_range_the_top_is_the_goal_and_the_middle_holds() {
        let mut curl = squat();
        curl.work = range(3, 8, 12);
        let default = targets(&curl, &[]);
        assert_eq!(
            default.working[0].goal,
            SetGoal::Reps {
                reps: Reps::new(12),
                range: Some(RepRange {
                    min: Reps::new(8),
                    max: Reps::new(12)
                })
            }
        );
        let fail = session(kg(100.0), &[8, 8, 7]);
        let hold = targets(&curl, &[fail.clone(), session(kg(100.0), &[12, 10, 9])]);
        assert_eq!(hold.last_verdict, Some(SessionVerdict::Hold));
        assert_eq!(working(&hold), (kg(100.0), Reps::new(12)));
        assert_eq!(hold.failed_sessions, 0, "a hold ends the failure streak");
        assert_eq!(describe(&hold, Unit::Kg), "Squat: stays at 100 kg");
        let top = targets(&curl, &[session(kg(100.0), &[12, 12, 12])]);
        assert_eq!(working(&top), (kg(102.5), Reps::new(12)));
    }

    #[test]
    fn pounds_round_to_five() {
        let mut press = squat();
        press.load = Some(Load::Weight(unit_weight(95.0, Unit::Lb)));
        press.progression = ProgressionRule::AddWhenTopOfRange {
            increment: unit_weight(5.0, Unit::Lb),
            deload_after_failures: deload(1, 10.0),
        };
        let settings = ProgressionSettings::for_unit(Unit::Lb);
        let up = ready(next_targets(
            &press,
            None,
            settings,
            &[session(lb(95.0), &[5, 5, 5])],
        ));
        assert_eq!(working(&up).0, lb(100.0));
        assert_eq!(describe(&up, Unit::Lb), "Squat: 95 → 100 lb");
        // 135 lb − 10 % = 121.5 lb → 120 lb.
        let down = ready(next_targets(
            &press,
            None,
            settings,
            &[session(lb(135.0), &[5, 5, 1])],
        ));
        assert_eq!(working(&down).0, lb(120.0));
        assert_eq!(describe(&down, Unit::Lb), "Squat: deload 135 → 120 lb");
    }

    #[test]
    fn a_custom_step() {
        let mut press = squat();
        press.progression = ProgressionRule::AddWhenTopOfRange {
            increment: unit_weight(1.0, Unit::Kg),
            deload_after_failures: None,
        };
        let history = [session(kg(40.0), &[5, 5, 5])];
        // With the default 2.5 kg step, +1 kg still moves to the next step.
        assert_eq!(working(&targets(&press, &history)).0, kg(42.5));
        // With 1 kg micro-plates, exactly +1 kg.
        let fine = ProgressionSettings::new(Unit::Kg, kg(1.0)).unwrap();
        let next = ready(next_targets(&press, None, fine, &history));
        assert_eq!(working(&next).0, kg(41.0));
    }
}

mod double_progression {
    use super::*;

    #[test]
    fn starts_at_the_bottom_of_the_range() {
        let next = targets(&row(), &[]);
        assert_eq!(next.source, TargetSource::ProgramDefault);
        assert_eq!(working(&next), (kg(50.0), Reps::new(8)));
    }

    #[test]
    fn reps_climb_then_the_weight_goes_up() {
        let steps: [(&[u16], f64, u16, &str); 4] = [
            (&[8, 8, 8], 50.0, 9, "Row: reps 8 → 9 at 50 kg"),
            (&[11, 10, 9], 50.0, 10, "Row: reps 9 → 10 at 50 kg"),
            (&[12, 12, 11], 50.0, 12, "Row: reps 11 → 12 at 50 kg"),
            (&[12, 12, 12], 52.5, 8, "Row: 50 → 52.5 kg, reps 12 → 8"),
        ];
        let mut history = Vec::new();
        for (reps, weight, aim, description) in steps {
            history.push(session(kg(50.0), reps));
            let next = targets(&row(), &history);
            assert_eq!(
                working(&next),
                (kg(weight), Reps::new(aim)),
                "{description}"
            );
            assert_eq!(describe(&next, Unit::Kg), description);
            assert_eq!(next.failed_sessions, 0);
        }
        let last = targets(&row(), &history);
        assert_eq!(last.last_verdict, Some(SessionVerdict::Success));
        assert_eq!(
            change(&last),
            ChangeKind::WeightIncreaseRepsReset {
                from: kg(50.0),
                to: kg(52.5),
                reps_from: Reps::new(12),
                reps_to: Reps::new(8)
            }
        );
    }

    #[test]
    fn the_best_sets_count_and_extra_sets_do_not_hurt() {
        let next = targets(&row(), &[session(kg(50.0), &[10, 5, 10, 10])]);
        assert_eq!(working(&next), (kg(50.0), Reps::new(11)));
    }

    #[test]
    fn a_set_below_the_range_is_a_failure_and_resets_the_reps() {
        let history = [
            session(kg(50.0), &[10, 10, 10]),
            session(kg(50.0), &[10, 9, 7]),
        ];
        let next = targets(&row(), &history);
        assert_eq!(next.last_verdict, Some(SessionVerdict::Failure));
        assert_eq!(working(&next), (kg(50.0), Reps::new(8)));
        assert_eq!(next.failed_sessions, 1);
    }

    #[test]
    fn deload_then_a_fresh_streak() {
        let fail = || session(kg(50.0), &[8, 8, 6]);
        let deloaded = targets(&row(), &[fail(), fail()]);
        assert_eq!(working(&deloaded), (kg(45.0), Reps::new(8)));
        assert_eq!(describe(&deloaded, Unit::Kg), "Row: deload 50 → 45 kg");
        let again = targets(&row(), &[fail(), fail(), session(kg(45.0), &[8, 8, 6])]);
        assert_eq!(working(&again).0, kg(45.0));
        assert_eq!(again.failed_sessions, 1);
    }
}

mod training_max {
    use super::*;

    fn with_tm(training_max: Weight, history: &[PastSession]) -> ExerciseTargets {
        ready(next_targets(
            &bench(),
            Some(training_max),
            kg_settings(),
            history,
        ))
    }

    #[test]
    fn needs_a_training_max() {
        let outcome = next_targets(&bench(), None, kg_settings(), &[]);
        assert_eq!(
            outcome,
            NextTargets::NeedsTrainingMax {
                exercise: bench().id
            }
        );
        assert_eq!(outcome.ready(), None);
        assert_eq!(outcome.exercise(), &bench().id);
        // Whatever the rule.
        let mut no_rule = bench();
        no_rule.progression = ProgressionRule::None;
        assert!(matches!(
            next_targets(&no_rule, None, kg_settings(), &[session(kg(80.0), &[5])]),
            NextTargets::NeedsTrainingMax { .. }
        ));
    }

    #[test]
    fn no_history_uses_the_percentage() {
        let next = with_tm(kg(100.0), &[]);
        assert_eq!(next.source, TargetSource::ProgramDefault);
        assert_eq!(working(&next), (kg(80.0), Reps::new(5)));
        assert_eq!(next.training_max, Some(kg(100.0)));
        assert_eq!(next.change, None);
        let outcome = next_targets(&bench(), Some(kg(100.0)), kg_settings(), &[]);
        assert_eq!(outcome.ready(), Some(&next));
        assert_eq!(outcome.exercise(), &bench().id);
    }

    #[test]
    fn success_raises_the_training_max() {
        let next = with_tm(kg(100.0), &[session(kg(80.0), &[5, 5, 5])]);
        assert_eq!(next.source, TargetSource::Progression);
        assert_eq!(next.training_max, Some(kg(102.5)));
        // 80 % of 102.5 kg is 82 kg: 82.5 kg to the nearest step.
        assert_eq!(working(&next).0, kg(82.5));
        assert_eq!(
            describe(&next, Unit::Kg),
            "Bench: training max 100 → 102.5 kg"
        );
    }

    #[test]
    fn replaying_is_idempotent() {
        let history = [session(kg(80.0), &[5, 5, 5]), session(kg(82.5), &[5, 5, 5])];
        let first = with_tm(kg(100.0), &history);
        let second = with_tm(kg(100.0), &history);
        assert_eq!(first, second);
        assert_eq!(first.training_max, Some(kg(105.0)));
        assert_eq!(working(&first).0, kg(85.0));
    }

    #[test]
    fn lighter_than_the_target_does_not_count() {
        let next = with_tm(kg(100.0), &[session(kg(70.0), &[5, 5, 5])]);
        assert_eq!(next.last_verdict, Some(SessionVerdict::Failure));
        assert_eq!(next.training_max, Some(kg(100.0)));
        assert_eq!(
            describe(&next, Unit::Kg),
            "Bench: training max stays at 100 kg (1 failed session)"
        );
        // Heavier is fine.
        let heavier = with_tm(kg(100.0), &[session(kg(85.0), &[5, 5, 5])]);
        assert_eq!(heavier.training_max, Some(kg(102.5)));
    }

    #[test]
    fn half_a_step_of_tolerance_on_the_target() {
        // 80 % of 102.5 kg is exactly 82 kg; the target shown was 82.5 kg.
        let history = [session(kg(80.0), &[5, 5, 5]), session(kg(82.0), &[5, 5, 5])];
        assert_eq!(with_tm(kg(100.0), &history).training_max, Some(kg(105.0)));
        // A lifter in lb loaded 180 lb (81.65 kg): within half a 2.5 kg step of 82 kg.
        let pounds = [
            session(kg(80.0), &[5, 5, 5]),
            session(lb(180.0), &[5, 5, 5]),
        ];
        assert_eq!(with_tm(kg(100.0), &pounds).training_max, Some(kg(105.0)));
        // A full step lighter does not count.
        let lighter = [session(kg(80.0), &[5, 5, 5]), session(kg(80.0), &[5, 5, 5])];
        let next = with_tm(kg(100.0), &lighter);
        assert_eq!(next.training_max, Some(kg(102.5)));
        assert_eq!(next.last_verdict, Some(SessionVerdict::Failure));
    }

    #[test]
    fn deload_cuts_the_training_max_then_the_count_restarts() {
        let fail = |weight| session(kg(weight), &[5, 5, 2]);
        let next = with_tm(kg(100.0), &[fail(80.0), fail(80.0)]);
        assert_eq!(next.training_max, Some(kg(90.0)));
        assert_eq!(working(&next).0, kg(72.5), "80 % of 90 kg is 72 kg");
        assert_eq!(
            describe(&next, Unit::Kg),
            "Bench: deload, training max 100 → 90 kg"
        );
        let again = with_tm(kg(100.0), &[fail(80.0), fail(80.0), fail(72.5)]);
        assert_eq!(again.training_max, Some(kg(90.0)));
        assert_eq!(again.failed_sessions, 1);
    }

    #[test]
    fn with_a_range_the_middle_holds() {
        let mut bench = bench();
        bench.work = range(3, 3, 5);
        let next = ready(next_targets(
            &bench,
            Some(kg(100.0)),
            kg_settings(),
            &[session(kg(80.0), &[5, 4, 3])],
        ));
        assert_eq!(next.last_verdict, Some(SessionVerdict::Hold));
        assert_eq!(next.training_max, Some(kg(100.0)));
        assert_eq!(working(&next), (kg(80.0), Reps::new(5)));
    }

    #[test]
    fn pounds() {
        let mut bench = bench();
        bench.progression = ProgressionRule::TrainingMax {
            increment: unit_weight(5.0, Unit::Lb),
            deload_after_failures: None,
        };
        bench.load = Some(Load::PercentOfTrainingMax(pct(75.0)));
        let settings = ProgressionSettings::for_unit(Unit::Lb);
        // 75 % of 225 lb is 168.75 lb: 170 lb.
        let first = ready(next_targets(&bench, Some(lb(225.0)), settings, &[]));
        assert_eq!(working(&first).0, lb(170.0));
        let next = ready(next_targets(
            &bench,
            Some(lb(225.0)),
            settings,
            &[session(lb(170.0), &[5, 5, 5])],
        ));
        assert_eq!(next.training_max, Some(lb(230.0)));
        // 75 % of 230 lb is 172.5 lb: 175 lb (halfway rounds up).
        assert_eq!(working(&next).0, lb(175.0));
    }
}

mod no_rule {
    use super::*;

    fn pullup() -> Exercise {
        exercise("Pull-up", range(3, 5, 10), None, ProgressionRule::None)
    }

    #[test]
    fn program_default_then_last_performance() {
        let press = exercise(
            "Press",
            fixed(3, 5),
            Some(Load::Weight(unit_weight(40.0, Unit::Kg))),
            ProgressionRule::None,
        );
        let first = targets(&press, &[]);
        assert_eq!(first.source, TargetSource::ProgramDefault);
        assert_eq!(working(&first), (kg(40.0), Reps::new(5)));

        let last = PastSession::new(vec![
            WorkingSet::new(kg(42.0), Reps::new(5)),
            WorkingSet::new(kg(41.0), Reps::new(3)),
        ]);
        let next = targets(&press, &[session(kg(30.0), &[5, 5, 5]), last]);
        assert_eq!(next.source, TargetSource::LastPerformance);
        let weights: Vec<_> = next.working.iter().map(|set| set.weight).collect();
        // Set by set, the last one repeated for the missing third set.
        assert_eq!(weights, [Some(kg(42.0)), Some(kg(41.0)), Some(kg(41.0))]);
        // A fixed count stays the program's.
        assert!(next.working.iter().all(|set| set.goal
            == SetGoal::Reps {
                reps: Reps::new(5),
                range: None
            }));
        assert_eq!(next.change, None);
        assert_eq!(next.last_verdict, None);
    }

    #[test]
    fn bodyweight_reps_are_clamped_into_the_range() {
        let first = targets(&pullup(), &[]);
        assert_eq!(first.working[0].weight, None);
        assert_eq!(
            first.working[0].goal,
            SetGoal::Reps {
                reps: Reps::new(10),
                range: Some(RepRange {
                    min: Reps::new(5),
                    max: Reps::new(10)
                })
            }
        );
        let last = PastSession::new(
            [12, 7, 2]
                .map(|r| WorkingSet::bodyweight(Reps::new(r)))
                .to_vec(),
        );
        let next = targets(&pullup(), &[last]);
        let reps: Vec<_> = next
            .working
            .iter()
            .map(|set| match set.goal {
                SetGoal::Reps { reps, .. } => reps.get(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(reps, [10, 7, 5]);
        assert!(next.working.iter().all(|set| set.weight.is_none()));
    }

    #[test]
    fn a_weight_logged_on_bodyweight_work_is_kept() {
        let last = PastSession::new(vec![WorkingSet::new(kg(10.0), Reps::new(6))]);
        let next = targets(&pullup(), &[last]);
        assert!(next.working.iter().all(|set| set.weight == Some(kg(10.0))));
    }

    #[test]
    fn percent_of_training_max_without_a_rule() {
        let mut press = bench();
        press.progression = ProgressionRule::None;
        let next = ready(next_targets(&press, Some(kg(60.0)), kg_settings(), &[]));
        assert_eq!(next.source, TargetSource::ProgramDefault);
        assert_eq!(working(&next).0, kg(47.5), "80 % of 60 kg is 48 kg");
        assert_eq!(next.training_max, Some(kg(60.0)));
        let later = ready(next_targets(
            &press,
            Some(kg(60.0)),
            kg_settings(),
            &[session(kg(50.0), &[5, 5, 5])],
        ));
        assert_eq!(later.source, TargetSource::LastPerformance);
        assert_eq!(working(&later).0, kg(50.0));
    }
}

mod timed {
    use super::*;

    #[test]
    fn holds_keep_the_program_duration() {
        let plank = exercise(
            "Plank",
            Work::Hold {
                sets: 3,
                seconds: Seconds::new(45),
            },
            None,
            ProgressionRule::None,
        );
        let hold = SetTarget {
            weight: None,
            goal: SetGoal::Hold {
                seconds: Seconds::new(45),
            },
        };
        let first = targets(&plank, &[]);
        assert_eq!(first.source, TargetSource::ProgramDefault);
        assert_eq!(first.working, vec![hold; 3]);
        // A short hold last time does not shorten the target.
        let short = PastSession::new(vec![
            WorkingSet {
                reps: Reps::new(1),
                weight: None,
                duration: Some(Seconds::new(30)),
            };
            3
        ]);
        let next = targets(&plank, &[short]);
        assert_eq!(next.source, TargetSource::LastPerformance);
        assert_eq!(next.working, vec![hold; 3]);
        assert_eq!(next.change, None);
        assert!(next.warmup.is_empty());
    }

    #[test]
    fn weighted_holds_take_the_last_weight() {
        let carry = exercise(
            "Carry",
            Work::Hold {
                sets: 2,
                seconds: Seconds::new(40),
            },
            Some(Load::Weight(unit_weight(24.0, Unit::Kg))),
            ProgressionRule::None,
        );
        assert_eq!(targets(&carry, &[]).working[0].weight, Some(kg(24.0)));
        let last = PastSession::new(vec![WorkingSet {
            reps: Reps::new(1),
            weight: Some(kg(32.0)),
            duration: Some(Seconds::new(40)),
        }]);
        let next = targets(&carry, &[last]);
        assert!(next.working.iter().all(|set| set.weight == Some(kg(32.0))));
    }

    #[test]
    fn intervals_are_one_target() {
        let sprints = exercise(
            "Sprints",
            Work::Intervals {
                work: Seconds::new(30),
                rest: Seconds::new(90),
                rounds: 8,
            },
            None,
            ProgressionRule::None,
        );
        let expected = vec![SetTarget {
            weight: None,
            goal: SetGoal::Intervals {
                work: Seconds::new(30),
                rest: Seconds::new(90),
                rounds: 8,
            },
        }];
        assert_eq!(targets(&sprints, &[]).working, expected);
        let done = PastSession::new(vec![WorkingSet {
            reps: Reps::new(6),
            weight: None,
            duration: Some(Seconds::new(180)),
        }]);
        assert_eq!(targets(&sprints, &[done]).working, expected);
    }

    #[test]
    fn a_rule_on_timed_work_is_ignored() {
        // Rejected by program validation; the engine still answers.
        let plank = exercise(
            "Plank",
            Work::Hold {
                sets: 1,
                seconds: Seconds::new(60),
            },
            Some(Load::Weight(unit_weight(10.0, Unit::Kg))),
            ProgressionRule::AddWhenTopOfRange {
                increment: unit_weight(2.5, Unit::Kg),
                deload_after_failures: None,
            },
        );
        let done = PastSession::new(vec![WorkingSet {
            reps: Reps::new(1),
            weight: Some(kg(10.0)),
            duration: Some(Seconds::new(60)),
        }]);
        let next = targets(&plank, &[done]);
        assert_eq!(next.source, TargetSource::LastPerformance);
        assert_eq!(next.working[0].weight, Some(kg(10.0)));
        assert_eq!(next.change, None);
    }
}

mod warmups {
    use super::*;

    fn with_warmup(mut exercise: Exercise) -> Exercise {
        exercise.warmup = vec![
            WarmupSet {
                sets: 2,
                reps: Reps::new(5),
                load: WarmupLoad::Weight(unit_weight(20.0, Unit::Kg)),
            },
            WarmupSet {
                sets: 1,
                reps: Reps::new(5),
                load: WarmupLoad::PercentOfWorkingWeight(pct(60.0)),
            },
            WarmupSet {
                sets: 1,
                reps: Reps::new(3),
                load: WarmupLoad::PercentOfWorkingWeight(pct(80.0)),
            },
        ];
        exercise
    }

    fn weights(targets: &ExerciseTargets) -> Vec<Option<Weight>> {
        targets.warmup.iter().map(|set| set.weight).collect()
    }

    #[test]
    fn follow_the_progressed_working_weight() {
        let squat = with_warmup(squat());
        let first = targets(&squat, &[]);
        assert_eq!(
            weights(&first),
            [
                Some(kg(20.0)),
                Some(kg(20.0)),
                Some(kg(60.0)),
                Some(kg(80.0))
            ]
        );
        assert_eq!(
            first.warmup[3].goal,
            SetGoal::Reps {
                reps: Reps::new(3),
                range: None
            }
        );
        // 102.5 kg: 61.5 → 62.5 kg and 82 → 82.5 kg.
        let next = targets(&squat, &[session(kg(100.0), &[5, 5, 5])]);
        assert_eq!(
            weights(&next),
            [
                Some(kg(20.0)),
                Some(kg(20.0)),
                Some(kg(62.5)),
                Some(kg(82.5))
            ]
        );
    }

    #[test]
    fn follow_the_training_max() {
        let bench = with_warmup(bench());
        let next = ready(next_targets(&bench, Some(kg(100.0)), kg_settings(), &[]));
        assert_eq!(
            weights(&next),
            [
                Some(kg(20.0)),
                Some(kg(20.0)),
                Some(kg(47.5)),
                Some(kg(65.0))
            ]
        );
        assert!(matches!(
            next_targets(&bench, None, kg_settings(), &[]),
            NextTargets::NeedsTrainingMax { .. }
        ));
    }

    #[test]
    fn fixed_warmups_round_to_the_step() {
        let squat = with_warmup(squat());
        let settings = ProgressionSettings::for_unit(Unit::Lb);
        let next = ready(next_targets(&squat, None, settings, &[]));
        // 100 kg is 220.46 lb: 220 lb, and the 20 kg bar is 45 lb.
        assert_eq!(working(&next).0, lb(220.0));
        assert_eq!(
            weights(&next),
            [
                Some(lb(45.0)),
                Some(lb(45.0)),
                Some(lb(130.0)),
                Some(lb(175.0))
            ]
        );
        // Rounding would make the warm-up as heavy as the working weight: kept as written.
        let mut light = with_warmup(squat.clone());
        light.load = Some(Load::Weight(unit_weight(45.0, Unit::Lb)));
        let next = ready(next_targets(&light, None, settings, &[]));
        assert_eq!(working(&next).0, lb(45.0));
        assert_eq!(next.warmup[0].weight, Some(kg(20.0)));
    }

    #[test]
    fn percentages_need_a_working_weight() {
        // A percentage warm-up on body-weight work is rejected by validation; no weight here.
        let pullup = with_warmup(exercise(
            "Pull-up",
            fixed(3, 5),
            None,
            ProgressionRule::None,
        ));
        assert_eq!(
            weights(&targets(&pullup, &[])),
            [Some(kg(20.0)), Some(kg(20.0)), None, None]
        );
    }

    #[test]
    fn never_as_heavy_as_a_light_working_weight() {
        let mut light = with_warmup(squat());
        light.load = Some(Load::Weight(unit_weight(22.5, Unit::Kg)));
        light.warmup[2].load = WarmupLoad::PercentOfWorkingWeight(pct(95.0));
        let next = targets(&light, &[]);
        // 95 % of 22.5 kg is 21.375 kg: 20 kg rather than 22.5 kg.
        assert_eq!(next.warmup[3].weight, Some(kg(20.0)));
    }
}

mod history_from_logs {
    use uuid::Uuid;

    use super::*;

    fn logged(
        n: u128,
        exercise: &ExerciseId,
        index: u16,
        reps: u16,
        weight: f64,
    ) -> LoggedSet<i64> {
        LoggedSet {
            id: SetId::from_uuid(Uuid::from_u128(n)),
            exercise: exercise.clone(),
            set_index: index,
            reps: Reps::new(reps),
            weight: Some(kg(weight)),
            duration: None,
            warm_up: false,
            completed_at: n as i64,
        }
    }

    #[test]
    fn end_to_end() {
        let squat = squat();
        let mut logs = Vec::new();
        for (n, weight) in [(1_u128, 100.0), (2, 102.5)] {
            let start = (n as i64) * 1_000;
            let mut log = SessionLog::start(
                SessionId::from_uuid(Uuid::from_u128(n)),
                ProgramVersionId::from_uuid(Uuid::from_u128(9)),
                DayId::new("a").unwrap(),
                start,
            );
            for index in 0..3 {
                let id = n * 100 + u128::from(index);
                let mut set = logged(id, &squat.id, index, 5, weight);
                set.completed_at = start + i64::from(index) + 1;
                log.add_set(set).unwrap();
            }
            log.complete(start + 10).unwrap();
            logs.push(log);
        }
        let history = exercise_history(&squat.id, &logs);
        assert_eq!(history.len(), 2);
        let next = targets(&squat, &history);
        assert_eq!(working(&next).0, kg(105.0));
        assert_eq!(describe(&next, Unit::Kg), "Squat: 102.5 → 105 kg");
    }
}

#[test]
fn every_builtin_exercise_has_loadable_targets() {
    for (builtin, unit) in builtin_programs()
        .unwrap()
        .into_iter()
        .flat_map(|builtin| Unit::ALL.map(|unit| (builtin.clone(), unit)))
    {
        let settings = ProgressionSettings::for_unit(unit);
        let step = settings.step();
        let program = builtin.program();
        let training_maxes = program.training_max_exercises();
        for exercise in program.exercises() {
            let training_max = training_maxes.contains(&exercise.id).then(|| kg(100.0));
            let next = ready(next_targets(exercise, training_max, settings, &[]));
            assert_eq!(next.source, TargetSource::ProgramDefault, "{}", exercise.id);
            assert_eq!(
                next.working.len(),
                usize::from(exercise.work.sets()),
                "{}",
                exercise.id
            );
            let on_step = |weight: Weight| {
                weight.round_to(step, iron_oxide_domain::Rounding::Down) == Ok(weight)
            };
            for set in next.working.iter().chain(&next.warmup) {
                if let Some(weight) = set.weight {
                    assert!(on_step(weight), "{} in {unit}: {weight:?}", exercise.id);
                }
            }
            let working = next.working.iter().filter_map(|set| set.weight).max();
            for warmup in &next.warmup {
                if let (Some(warmup), Some(working)) = (warmup.weight, working) {
                    assert!(warmup < working, "{} in {unit}", exercise.id);
                }
            }
        }
    }
}
