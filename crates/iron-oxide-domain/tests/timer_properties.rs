//! Property tests: timer alerts and phase changes fire at most once and are never lost, whatever
//! the clock does, across user actions and reloads.

use std::time::Duration;

use iron_oxide_domain::time::Timestamp;
use iron_oxide_domain::timer::{
    ADJUSTMENT_STEP, AlertThresholds, HoldTimer, IntervalEvent, IntervalPhase, IntervalPlan,
    IntervalTimer, RestTimer, TimerAlert,
};
use proptest::prelude::*;

const START: i64 = 1_700_000_000_000;

#[derive(Debug, Clone, Copy)]
enum Op {
    /// Moves the clock by this many milliseconds (possibly backwards) and observes.
    Observe(i64),
    /// Moves the clock by this many milliseconds (possibly backwards) without observing.
    Step(i64),
    Add,
    Subtract,
    Skip,
    /// Serializes the timer and reads it back, as a page reload would.
    Reload,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (-30_000_i64..120_000).prop_map(Op::Observe),
        2 => (-60_000_i64..60_000).prop_map(Op::Step),
        1 => Just(Op::Add),
        1 => Just(Op::Subtract),
        1 => Just(Op::Skip),
        1 => Just(Op::Reload),
    ]
}

/// Serializes `timer` and reads it back, as a page reload would.
fn reload<T: serde::Serialize + serde::de::DeserializeOwned>(
    timer: &T,
) -> Result<T, TestCaseError> {
    let fail = |error: serde_json::Error| TestCaseError::fail(error.to_string());
    serde_json::from_str(&serde_json::to_string(timer).map_err(fail)?).map_err(fail)
}

/// The alert level a rest reaches at `now`: 0 nothing, 1 warning, 2 finished.
fn level(ends_at: i64, warning_ms: Option<u64>, now: i64) -> u8 {
    let remaining = u64::try_from(ends_at - now).unwrap_or(0);
    if remaining == 0 {
        2
    } else if warning_ms.is_some_and(|warning| remaining <= warning) {
        1
    } else {
        0
    }
}

fn thresholds() -> impl Strategy<Value = AlertThresholds> {
    prop_oneof![
        Just(AlertThresholds::default()),
        Just(AlertThresholds::without_warning()),
        (0_u64..200_000).prop_map(|ms| AlertThresholds::warning_before(Duration::from_millis(ms))),
    ]
}

fn at(millis: i64) -> Timestamp {
    Timestamp::from_epoch_millis(millis)
}

proptest! {
    /// Whatever the clock does (steps back and forth, user actions and reloads in between):
    /// - safety: in each arming (start or adjustment), each alert fires at most once and nothing
    ///   follows `Finished`;
    /// - liveness: alerts match an independent model of "fire when the level reached is past
    ///   what this arming announced", and an arming that was not already over finishes exactly
    ///   once when an observation reaches its end.
    #[test]
    fn rest_alerts_fire_once_per_arming_and_are_never_lost(
        duration_ms in 0_u64..600_000,
        thresholds in thresholds(),
        ops in prop::collection::vec(op(), 0..80),
    ) {
        let warning_ms = thresholds.warning().map(|w| u64::try_from(w.as_millis()).unwrap());
        let mut now = START;
        let mut timer = RestTimer::start_with_thresholds(at(now), Duration::from_millis(duration_ms), thresholds);
        let level_of = |timer: &RestTimer, now: i64| level(timer.ends_at().epoch_millis(), warning_ms, now);
        let mut arm_level = level_of(&timer, now);
        let mut model_level = arm_level;
        let mut fired: Vec<TimerAlert> = Vec::new();
        let mut expected: Vec<TimerAlert> = Vec::new();

        // The generated ops, then a last observation exactly at the end of the current arming.
        for op in ops.into_iter().map(Some).chain([None]) {
            let op = op.unwrap_or(Op::Observe(timer.ends_at().epoch_millis() - now));
            match op {
                Op::Observe(delta) => {
                    now += delta;
                    let reached = level_of(&timer, now);
                    if reached > model_level {
                        model_level = reached;
                        expected.push(if reached == 2 { TimerAlert::Finished } else { TimerAlert::Warning });
                    }
                    if let Some(alert) = timer.observe(at(now)) {
                        prop_assert!(!fired.contains(&alert), "{alert:?} fired twice: {fired:?}");
                        prop_assert!(!fired.contains(&TimerAlert::Finished), "{alert:?} after Finished");
                        fired.push(alert);
                    }
                    prop_assert_eq!(&fired, &expected);
                }
                Op::Step(delta) => now += delta,
                Op::Reload => {
                    let restored: RestTimer = reload(&timer)?;
                    prop_assert_eq!(restored, timer);
                    timer = restored;
                }
                Op::Add | Op::Subtract | Op::Skip => {
                    let before = timer.ends_at();
                    match op {
                        Op::Add => {
                            timer.add(at(now), ADJUSTMENT_STEP);
                            prop_assert!(timer.ends_at() >= before);
                            prop_assert!(timer.remaining(at(now)) >= ADJUSTMENT_STEP);
                        }
                        Op::Subtract => {
                            timer.subtract(at(now), ADJUSTMENT_STEP);
                            prop_assert!(timer.ends_at() <= before);
                            if before > at(now) {
                                prop_assert!(timer.ends_at() >= at(now), "ended before now");
                            } else {
                                prop_assert_eq!(timer.ends_at(), before);
                            }
                        }
                        _ => {
                            timer.skip(at(now));
                            prop_assert!(timer.is_finished(at(now)));
                        }
                    }
                    // A new arming: what is reached at the action's own instant is silent.
                    arm_level = level_of(&timer, now);
                    model_level = arm_level;
                    fired.clear();
                    expected.clear();
                }
            }
            prop_assert!(timer.started_at() <= timer.ends_at());
        }
        // The last observation reached the end: an arming that was running finished, once.
        prop_assert_eq!(fired.contains(&TimerAlert::Finished), arm_level < 2);
    }

    /// With a monotonic clock and no adjustment, a rest that is observed after its end finishes
    /// exactly once, and warns iff an observation landed inside the warning window first.
    #[test]
    fn rest_alerts_are_never_missed_with_a_monotonic_clock(
        duration_ms in 0_u64..600_000,
        warning_ms in 0_u64..200_000,
        steps in prop::collection::vec(0_i64..90_000, 0..40),
    ) {
        let mut timer = RestTimer::start_with_thresholds(
            at(START),
            Duration::from_millis(duration_ms),
            AlertThresholds::warning_before(Duration::from_millis(warning_ms)),
        );
        let end = START + i64::try_from(duration_ms).unwrap();
        let mut now = START;
        let mut times: Vec<i64> = steps.iter().map(|step| { now += step; now }).collect();
        times.push(end.max(now));

        let alerts: Vec<(i64, TimerAlert)> =
            times.iter().filter_map(|&t| timer.observe(at(t)).map(|alert| (t, alert))).collect();

        let finished: Vec<i64> = alerts.iter().filter(|(_, a)| *a == TimerAlert::Finished).map(|(t, _)| *t).collect();
        if duration_ms == 0 {
            prop_assert!(finished.is_empty(), "a zero rest finishes silently");
        } else {
            prop_assert_eq!(finished.len(), 1);
        }

        let warning_window = |t: i64| {
            let remaining = u64::try_from(end - t).unwrap_or(0);
            remaining > 0 && remaining <= warning_ms
        };
        let expect_warning = duration_ms > warning_ms && times.iter().any(|&t| warning_window(t));
        let warnings: Vec<i64> = alerts.iter().filter(|(_, a)| *a == TimerAlert::Warning).map(|(t, _)| *t).collect();
        prop_assert_eq!(warnings.len(), usize::from(expect_warning));
        if let (Some(warned), Some(done)) = (warnings.first(), finished.first()) {
            prop_assert!(warned < done);
        }
    }

    /// The hold timer shares the rest timer's alerts: at most once each, whatever the clock does.
    #[test]
    fn hold_alerts_fire_at_most_once(
        target_ms in 0_u64..300_000,
        deltas in prop::collection::vec(-30_000_i64..60_000, 0..60),
    ) {
        let mut now = START;
        let mut hold = HoldTimer::start(at(now), Duration::from_millis(target_ms));
        let mut fired: Vec<TimerAlert> = Vec::new();
        for delta in deltas {
            now += delta;
            hold = reload(&hold)?;
            if let Some(alert) = hold.observe(at(now)) {
                prop_assert!(!fired.contains(&alert));
                prop_assert!(!fired.contains(&TimerAlert::Finished));
                fired.push(alert);
            }
        }
    }

    /// Phase changes are raised at most once, in order, whatever the clock does.
    #[test]
    fn interval_events_fire_at_most_once_in_order(
        work_ms in 1_u64..60_000,
        rest_ms in 0_u64..60_000,
        rounds in 1_u32..8,
        deltas in prop::collection::vec(-30_000_i64..90_000, 0..80),
    ) {
        let plan = IntervalPlan::new(Duration::from_millis(work_ms), Duration::from_millis(rest_ms), rounds).unwrap();
        let mut now = START;
        let mut timer = IntervalTimer::start(at(now), plan);
        let mut last_rank = 0_u64;
        let mut completed = 0_usize;
        for delta in deltas {
            now += delta;
            timer = reload(&timer)?;
            if let Some(event) = timer.observe(at(now)) {
                let rank = match event {
                    IntervalEvent::WorkStarted { round } => {
                        prop_assert!(round >= 2 && round <= rounds);
                        u64::from(round - 1) * 2
                    }
                    IntervalEvent::RestStarted { round } => {
                        prop_assert!(round >= 1 && round < rounds);
                        u64::from(round - 1) * 2 + 1
                    }
                    IntervalEvent::Completed => u64::from(rounds) * 2 - 1,
                };
                prop_assert!(rank > last_rank, "{event:?} after rank {last_rank}");
                completed += usize::from(event == IntervalEvent::Completed);
                last_rank = rank;
            }
            let status = timer.status(at(now));
            prop_assert!(status.round >= 1 && status.round <= rounds);
            if status.phase == IntervalPhase::Done {
                prop_assert_eq!(status.remaining_in_phase, Duration::ZERO);
            } else {
                prop_assert!(status.remaining_in_phase > Duration::ZERO);
            }
        }
        // Liveness: once an observation reaches the end, the timer has completed, exactly once.
        let end = timer.ends_at();
        timer = reload(&timer)?;
        let completed_now = timer.observe(end) == Some(IntervalEvent::Completed);
        prop_assert_eq!(completed + usize::from(completed_now), 1);
    }

    /// With a monotonic clock that reaches the end, the timer completes exactly once.
    #[test]
    fn interval_completes_exactly_once_with_a_monotonic_clock(
        work_ms in 1_u64..60_000,
        rest_ms in 0_u64..60_000,
        rounds in 1_u32..8,
        steps in prop::collection::vec(0_i64..90_000, 0..40),
    ) {
        let plan = IntervalPlan::new(Duration::from_millis(work_ms), Duration::from_millis(rest_ms), rounds).unwrap();
        let mut timer = IntervalTimer::start(at(START), plan);
        let mut now = START;
        let mut times: Vec<i64> = steps.iter().map(|step| { now += step; now }).collect();
        times.push(timer.ends_at().epoch_millis().max(now));
        let completed = times
            .iter()
            .filter(|&&t| timer.observe(at(t)) == Some(IntervalEvent::Completed))
            .count();
        prop_assert_eq!(completed, 1);
    }
}
