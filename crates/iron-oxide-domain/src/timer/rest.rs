use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{AlertThresholds, Countdown, TimerAlert};
use crate::time::Timestamp;

/// The rest between two sets: a countdown to `ends_at` that the lifter can shorten, extend or
/// skip.
///
/// See the [module documentation](super) for how alerts behave across gaps, clock skew and
/// adjustments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RestTimer(Countdown);

impl RestTimer {
    /// Starts a rest of `duration` at `now`, warning 10 seconds before the end.
    #[must_use]
    pub fn start(now: Timestamp, duration: Duration) -> Self {
        Self::start_with_thresholds(now, duration, AlertThresholds::default())
    }

    /// Starts a rest of `duration` at `now` with custom alert thresholds.
    ///
    /// A rest that is already within the warning threshold when it starts does not warn.
    #[must_use]
    pub fn start_with_thresholds(
        now: Timestamp,
        duration: Duration,
        thresholds: AlertThresholds,
    ) -> Self {
        Self(Countdown::start(now, duration, thresholds))
    }

    /// When the rest started.
    #[must_use]
    pub fn started_at(&self) -> Timestamp {
        self.0.started_at
    }

    /// When the rest ends, adjustments included.
    #[must_use]
    pub fn ends_at(&self) -> Timestamp {
        self.0.ends_at
    }

    /// The whole length of the rest from its start to its current end, adjustments included.
    /// Useful to draw progress as `remaining / total`.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.0.total()
    }

    /// The thresholds this rest alerts at.
    #[must_use]
    pub fn thresholds(&self) -> AlertThresholds {
        self.0.thresholds()
    }

    /// Time left at `now`, zero once the rest is over.
    #[must_use]
    pub fn remaining(&self, now: Timestamp) -> Duration {
        self.0.remaining(now)
    }

    /// Whether the rest is over at `now` (the end instant included).
    #[must_use]
    pub fn is_finished(&self, now: Timestamp) -> bool {
        self.0.is_finished(now)
    }

    /// Adds `by` to the rest (the `+15s` button). Like every adjustment, it re-arms the alerts
    /// at `now`: whatever the new end already reaches at `now` is marked announced, silently.
    ///
    /// On a rest that is already over, the new rest lasts `by` from `now`. Thresholds the new end
    /// puts back ahead are re-armed: `+15s` with 8 seconds left warns again at 10 seconds.
    pub fn add(&mut self, now: Timestamp, by: Duration) {
        self.0.ends_at = self.0.ends_at.max(now).saturating_add(by);
        self.0.arm(now);
    }

    /// Removes `by` from the rest (the `-15s` button), never ending it before `now`.
    ///
    /// When less than `by` is left, the rest ends at `now`: it is finished, silently, like
    /// [`skip`](Self::skip). A rest that is already over is left unchanged. Jumping over the
    /// warning threshold does not warn.
    pub fn subtract(&mut self, now: Timestamp, by: Duration) {
        if self.0.ends_at > now {
            self.0.ends_at = self.0.ends_at.saturating_sub(by).max(now);
        }
        self.0.clamp_start();
        self.0.arm(now);
    }

    /// Ends the rest at `now` without raising [`TimerAlert::Finished`]. A rest that is already
    /// over keeps its end instant.
    pub fn skip(&mut self, now: Timestamp) {
        self.0.ends_at = self.0.ends_at.min(now);
        self.0.clamp_start();
        self.0.arm(now);
    }

    /// The alert crossed between `previous` (excluded) and `now` (included), if any, for the
    /// current end instant. Pure and stateless: it ignores what was already announced, so prefer
    /// [`observe`](Self::observe) to drive the UI.
    ///
    /// Returns `None` when `now` is not after `previous`. When both thresholds are crossed, only
    /// [`TimerAlert::Finished`] is returned.
    #[must_use]
    pub fn alert_between(&self, previous: Timestamp, now: Timestamp) -> Option<TimerAlert> {
        self.0.alert_between(previous, now)
    }

    /// The alert to raise now, if any, given what the current arming already announced. Call it
    /// on every UI tick and when the page becomes visible again.
    pub fn observe(&mut self, now: Timestamp) -> Option<TimerAlert> {
        self.0.observe(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::ADJUSTMENT_STEP;

    const START: i64 = 1_700_000_000_000;

    fn at(offset_ms: i64) -> Timestamp {
        Timestamp::from_epoch_millis(START + offset_ms)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn rest(duration_s: u64) -> RestTimer {
        RestTimer::start(at(0), secs(duration_s))
    }

    /// Observes at each offset and collects the alerts with the offset they fired at.
    fn observe_all(timer: &mut RestTimer, offsets: &[i64]) -> Vec<(i64, TimerAlert)> {
        offsets
            .iter()
            .filter_map(|&offset| timer.observe(at(offset)).map(|alert| (offset, alert)))
            .collect()
    }

    #[test]
    fn starts_with_the_full_duration() {
        let timer = rest(90);
        assert_eq!(timer.started_at(), at(0));
        assert_eq!(timer.ends_at(), at(90_000));
        assert_eq!(timer.total(), secs(90));
        assert_eq!(timer.thresholds(), AlertThresholds::default());
        assert_eq!(timer.remaining(at(0)), secs(90));
        assert!(!timer.is_finished(at(0)));
    }

    #[test]
    fn derives_remaining_from_the_end_timestamp() {
        let timer = rest(90);
        assert_eq!(timer.remaining(at(1)), Duration::from_millis(89_999));
        assert_eq!(timer.remaining(at(80_000)), secs(10));
        assert_eq!(timer.remaining(at(89_999)), Duration::from_millis(1));
        assert_eq!(timer.remaining(at(90_000)), Duration::ZERO);
    }

    #[test]
    fn remaining_saturates_at_zero_after_the_end() {
        let timer = rest(90);
        assert_eq!(timer.remaining(at(90_001)), Duration::ZERO);
        assert_eq!(timer.remaining(at(3_600_000)), Duration::ZERO);
        assert_eq!(
            timer.remaining(Timestamp::from_epoch_millis(i64::MAX)),
            Duration::ZERO
        );
    }

    #[test]
    fn remaining_after_a_simulated_screen_lock() {
        let mut timer = rest(120);
        assert_eq!(timer.observe(at(1_000)), None);
        // Locked for 45 seconds: nothing was observed, the remaining time is still exact.
        assert_eq!(timer.remaining(at(46_000)), secs(74));
        assert_eq!(timer.observe(at(46_000)), None);
    }

    #[test]
    fn remaining_before_the_start_is_capped_by_the_end() {
        // A clock that jumped backwards shows more than the planned rest, never a panic.
        let timer = rest(90);
        assert_eq!(timer.remaining(at(-5_000)), secs(95));
        assert!(!timer.is_finished(at(-5_000)));
    }

    #[test]
    fn is_finished_from_the_end_instant() {
        let timer = rest(90);
        assert!(!timer.is_finished(at(89_999)));
        assert!(timer.is_finished(at(90_000)));
        assert!(timer.is_finished(at(90_001)));
    }

    #[test]
    fn zero_rest_is_finished_at_once_and_silent() {
        let mut timer = rest(0);
        assert!(timer.is_finished(at(0)));
        assert_eq!(timer.total(), Duration::ZERO);
        assert_eq!(observe_all(&mut timer, &[0, 1, 10_000]), vec![]);
    }

    #[test]
    fn warns_when_exactly_ten_seconds_are_left() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(79_999)), None);
        assert_eq!(timer.observe(at(80_000)), Some(TimerAlert::Warning));
        assert_eq!(timer.observe(at(80_001)), None);
    }

    #[test]
    fn finishes_when_exactly_zero_is_left() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(80_000)), Some(TimerAlert::Warning));
        assert_eq!(timer.observe(at(89_999)), None);
        assert_eq!(timer.observe(at(90_000)), Some(TimerAlert::Finished));
        assert_eq!(timer.observe(at(90_001)), None);
    }

    #[test]
    fn a_second_by_second_tick_fires_each_alert_once() {
        let mut timer = rest(90);
        let ticks: Vec<i64> = (0..=200).map(|s| s * 1_000).collect();
        assert_eq!(
            observe_all(&mut timer, &ticks),
            vec![
                (80_000, TimerAlert::Warning),
                (90_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn a_coarse_tick_fires_late_but_once() {
        let mut timer = rest(90);
        let ticks: Vec<i64> = (0..=13).map(|n| n * 7_000).chain([95_000]).collect();
        // 77s leaves 13s, 84s leaves 6s: the warning comes at the first tick past the threshold.
        assert_eq!(
            observe_all(&mut timer, &ticks),
            vec![
                (84_000, TimerAlert::Warning),
                (91_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn repeated_observations_at_the_same_instant_fire_once() {
        let mut timer = rest(90);
        assert_eq!(
            observe_all(&mut timer, &[80_000, 80_000, 80_000, 90_000, 90_000]),
            vec![
                (80_000, TimerAlert::Warning),
                (90_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn a_gap_over_both_thresholds_only_finishes() {
        // Screen locked for 5 minutes: one "rest over", no stale "10 seconds left".
        let mut timer = rest(90);
        assert_eq!(
            observe_all(&mut timer, &[1_000, 301_000, 302_000]),
            vec![(301_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn a_gap_over_the_warning_only_warns_late() {
        let mut timer = rest(90);
        assert_eq!(
            observe_all(&mut timer, &[1_000, 87_000, 88_000, 90_000]),
            vec![
                (87_000, TimerAlert::Warning),
                (90_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn a_huge_gap_does_not_overflow() {
        let mut timer = rest(90);
        assert_eq!(
            timer.observe(Timestamp::from_epoch_millis(i64::MAX)),
            Some(TimerAlert::Finished)
        );
        assert_eq!(timer.observe(Timestamp::from_epoch_millis(i64::MAX)), None);
    }

    #[test]
    fn a_clock_going_backwards_does_not_fire_twice() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(85_000)), Some(TimerAlert::Warning));
        // The clock jumps back 10 seconds, above the threshold again, then forward.
        assert_eq!(timer.observe(at(75_000)), None);
        assert_eq!(timer.remaining(at(75_000)), secs(15));
        assert_eq!(timer.observe(at(86_000)), None);
        assert_eq!(timer.observe(at(90_000)), Some(TimerAlert::Finished));
        assert_eq!(timer.observe(at(60_000)), None);
        assert_eq!(timer.observe(at(95_000)), None);
    }

    #[test]
    fn a_clock_skew_before_the_start_is_harmless() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(-60_000)), None);
        assert_eq!(
            observe_all(&mut timer, &[80_000, 90_000]),
            vec![
                (80_000, TimerAlert::Warning),
                (90_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn a_rest_shorter_than_the_warning_does_not_warn_at_start() {
        for duration_s in [1, 5, 10] {
            let mut timer = rest(duration_s);
            let end = i64::try_from(duration_s).unwrap() * 1_000;
            assert_eq!(
                observe_all(&mut timer, &[0, 1, end - 1, end, end + 1]),
                vec![(end, TimerAlert::Finished)],
                "rest of {duration_s}s"
            );
        }
    }

    #[test]
    fn a_rest_just_over_the_warning_warns() {
        let mut timer = RestTimer::start(at(0), Duration::from_millis(10_001));
        assert_eq!(
            observe_all(&mut timer, &[0, 1, 10_001]),
            vec![(1, TimerAlert::Warning), (10_001, TimerAlert::Finished)]
        );
    }

    #[test]
    fn the_warning_threshold_is_configurable() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(90),
            AlertThresholds::warning_before(secs(30)),
        );
        assert_eq!(timer.thresholds().warning(), Some(secs(30)));
        assert_eq!(
            observe_all(&mut timer, &[59_999, 60_000, 80_000, 90_000]),
            vec![
                (60_000, TimerAlert::Warning),
                (90_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn the_warning_can_be_disabled() {
        let mut timer =
            RestTimer::start_with_thresholds(at(0), secs(90), AlertThresholds::without_warning());
        assert_eq!(timer.thresholds().warning(), None);
        assert_eq!(
            observe_all(&mut timer, &[80_000, 90_000]),
            vec![(90_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn a_zero_warning_threshold_only_finishes() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(90),
            AlertThresholds::warning_before(Duration::ZERO),
        );
        assert_eq!(
            observe_all(&mut timer, &[89_999, 90_000, 90_001]),
            vec![(90_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn a_warning_threshold_longer_than_the_rest_never_warns() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(60),
            AlertThresholds::warning_before(secs(90)),
        );
        assert_eq!(
            observe_all(&mut timer, &[1, 30_000, 60_000]),
            vec![(60_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn a_huge_duration_saturates() {
        let mut timer = RestTimer::start(at(0), Duration::MAX);
        assert_eq!(timer.ends_at(), Timestamp::from_epoch_millis(i64::MAX));
        assert_eq!(timer.observe(at(1_000_000)), None);
        assert_eq!(
            RestTimer::start_with_thresholds(
                at(0),
                secs(1),
                AlertThresholds::warning_before(Duration::MAX)
            )
            .thresholds()
            .warning(),
            Some(Duration::from_millis(u64::MAX))
        );
    }

    #[test]
    fn add_extends_a_running_rest() {
        let mut timer = rest(90);
        timer.add(at(30_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(105_000));
        assert_eq!(timer.remaining(at(30_000)), secs(75));
        assert_eq!(timer.total(), secs(105));
        assert_eq!(timer.started_at(), at(0));
    }

    #[test]
    fn add_before_the_warning_warns_once_at_the_new_threshold() {
        let mut timer = rest(90);
        timer.add(at(30_000), ADJUSTMENT_STEP);
        assert_eq!(
            observe_all(&mut timer, &[80_000, 94_999, 95_000, 105_000]),
            vec![
                (95_000, TimerAlert::Warning),
                (105_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn add_after_the_warning_rearms_it() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(82_000)), Some(TimerAlert::Warning));
        timer.add(at(82_000), ADJUSTMENT_STEP); // 8s left becomes 23s.
        assert_eq!(timer.remaining(at(82_000)), secs(23));
        assert_eq!(
            observe_all(&mut timer, &[82_000, 90_000, 95_000, 105_000]),
            vec![
                (95_000, TimerAlert::Warning),
                (105_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn add_that_stays_under_the_warning_does_not_warn_again() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(90),
            AlertThresholds::warning_before(secs(30)),
        );
        assert_eq!(timer.observe(at(62_000)), Some(TimerAlert::Warning));
        timer.add(at(62_000), ADJUSTMENT_STEP); // 28s left becomes 43s: re-armed.
        assert_eq!(timer.observe(at(75_000)), Some(TimerAlert::Warning));
        timer.add(at(75_000), secs(1)); // 30s left becomes 31s: re-armed again.
        assert_eq!(timer.observe(at(76_000)), Some(TimerAlert::Warning));
        timer.add(at(80_000), Duration::ZERO); // 26s left: nothing to re-arm.
        assert_eq!(
            observe_all(&mut timer, &[80_000, 105_999, 106_000]),
            vec![(106_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn add_after_the_end_starts_from_now() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(100_000)), Some(TimerAlert::Finished));
        timer.add(at(120_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(135_000));
        assert_eq!(timer.remaining(at(120_000)), secs(15));
        assert!(!timer.is_finished(at(120_000)));
        assert_eq!(
            observe_all(&mut timer, &[120_000, 125_000, 135_000]),
            vec![
                (125_000, TimerAlert::Warning),
                (135_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn add_saturates() {
        let mut timer = rest(90);
        timer.add(at(0), Duration::MAX);
        assert_eq!(timer.ends_at(), Timestamp::from_epoch_millis(i64::MAX));
        assert_eq!(timer.observe(at(1_000)), None);
    }

    #[test]
    fn subtract_shortens_a_running_rest() {
        let mut timer = rest(90);
        timer.subtract(at(30_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(75_000));
        assert_eq!(timer.remaining(at(30_000)), secs(45));
        assert_eq!(timer.total(), secs(75));
        assert_eq!(
            observe_all(&mut timer, &[64_999, 65_000, 75_000]),
            vec![
                (65_000, TimerAlert::Warning),
                (75_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn subtract_leaving_exactly_zero_finishes_silently() {
        let mut timer = rest(90);
        timer.subtract(at(75_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(75_000));
        assert!(timer.is_finished(at(75_000)));
        assert_eq!(observe_all(&mut timer, &[75_000, 75_001, 100_000]), vec![]);
    }

    #[test]
    fn subtract_never_ends_before_now() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(82_000)), Some(TimerAlert::Warning));
        timer.subtract(at(82_000), ADJUSTMENT_STEP); // 8s left: the rest ends now.
        assert_eq!(timer.ends_at(), at(82_000));
        assert_eq!(timer.remaining(at(82_000)), Duration::ZERO);
        assert!(timer.is_finished(at(82_000)));
        assert_eq!(timer.total(), secs(82));
        assert_eq!(observe_all(&mut timer, &[82_000, 82_001, 90_000]), vec![]);
    }

    #[test]
    fn subtract_to_now_is_silent_whether_or_not_now_was_observed() {
        // Same outcome if the UI observed at the adjustment instant or not.
        for observe_first in [false, true] {
            let mut timer = rest(90);
            if observe_first {
                assert_eq!(timer.observe(at(85_000)), Some(TimerAlert::Warning));
            }
            timer.subtract(at(85_000), ADJUSTMENT_STEP);
            assert_eq!(
                observe_all(&mut timer, &[85_000, 85_001]),
                vec![],
                "observe first: {observe_first}"
            );
        }
    }

    #[test]
    fn subtract_jumping_over_the_warning_is_silent() {
        let mut timer = rest(90);
        timer.subtract(at(70_000), ADJUSTMENT_STEP); // 20s left becomes 5s.
        assert_eq!(timer.remaining(at(70_000)), secs(5));
        assert_eq!(
            observe_all(&mut timer, &[70_000, 71_000, 75_000]),
            vec![(75_000, TimerAlert::Finished)]
        );
    }

    #[test]
    fn subtract_after_the_end_changes_nothing() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(95_000)), Some(TimerAlert::Finished));
        timer.subtract(at(100_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(90_000));
        assert_eq!(timer.observe(at(101_000)), None);
    }

    #[test]
    fn subtract_saturates() {
        let mut timer = rest(90);
        timer.subtract(at(10_000), Duration::MAX);
        assert_eq!(timer.ends_at(), at(10_000));
    }

    #[test]
    fn add_then_subtract_restores_the_end() {
        let mut timer = rest(90);
        timer.add(at(20_000), ADJUSTMENT_STEP);
        timer.subtract(at(21_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(90_000));
    }

    #[test]
    fn skip_ends_the_rest_now_without_alerting() {
        let mut timer = rest(90);
        timer.skip(at(30_000));
        assert_eq!(timer.ends_at(), at(30_000));
        assert!(timer.is_finished(at(30_000)));
        assert_eq!(timer.remaining(at(30_000)), Duration::ZERO);
        assert_eq!(observe_all(&mut timer, &[30_000, 30_001, 200_000]), vec![]);
    }

    #[test]
    fn skip_after_the_warning_is_silent() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(81_000)), Some(TimerAlert::Warning));
        timer.skip(at(85_000));
        assert_eq!(observe_all(&mut timer, &[85_000, 90_000]), vec![]);
    }

    #[test]
    fn skip_after_the_end_keeps_the_end() {
        let mut timer = rest(90);
        timer.skip(at(120_000));
        assert_eq!(timer.ends_at(), at(90_000));
        assert_eq!(timer.observe(at(121_000)), None);
    }

    #[test]
    fn adjustments_during_clock_skew_arm_at_their_own_now() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(85_000)), Some(TimerAlert::Warning));
        timer.skip(at(50_000)); // The clock went back 35 seconds.
        assert_eq!(timer.ends_at(), at(50_000));
        assert_eq!(observe_all(&mut timer, &[60_000, 90_000]), vec![]);
        timer.add(at(40_000), ADJUSTMENT_STEP); // Back 50 seconds: ends at 65s, 25s left.
        assert_eq!(timer.ends_at(), at(65_000));
        timer.subtract(at(30_000), ADJUSTMENT_STEP); // Back again: ends at 50s, 20s left.
        assert_eq!(timer.ends_at(), at(50_000));
        // The rest armed by the lifter still warns and finishes once time passes its end.
        assert_eq!(
            observe_all(&mut timer, &[30_000, 40_000, 45_000, 50_000, 100_000]),
            vec![
                (40_000, TimerAlert::Warning),
                (50_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn subtract_after_a_backward_clock_step_still_finishes() {
        let mut timer = RestTimer::start(at(0), secs(60));
        assert_eq!(timer.observe(at(50_000)), Some(TimerAlert::Warning)); // 10s left.
        timer.subtract(at(30_000), ADJUSTMENT_STEP); // The clock stepped back 20s, then -15s.
        assert_eq!(timer.ends_at(), at(45_000)); // 15s left on screen.
        let alerts: Vec<TimerAlert> = (30_001..=200_000)
            .step_by(1_000)
            .filter_map(|t| timer.observe(at(t)))
            .collect();
        assert_eq!(alerts, vec![TimerAlert::Warning, TimerAlert::Finished]);
    }

    #[test]
    fn add_after_a_backward_clock_step_rearms() {
        let mut timer = RestTimer::start(at(0), secs(60));
        assert_eq!(timer.observe(at(60_000)), Some(TimerAlert::Finished));
        assert_eq!(timer.observe(at(100_000)), None);
        timer.add(at(70_000), ADJUSTMENT_STEP); // The clock stepped back 30s, then +15s.
        assert_eq!(timer.ends_at(), at(85_000));
        let alerts: Vec<TimerAlert> = (70_001..=200_000)
            .step_by(500)
            .filter_map(|t| timer.observe(at(t)))
            .collect();
        assert_eq!(alerts, vec![TimerAlert::Warning, TimerAlert::Finished]);
    }

    #[test]
    fn add_never_alerts_with_a_threshold_above_the_step() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(30),
            AlertThresholds::warning_before(secs(20)),
        );
        assert_eq!(timer.observe(at(1_000)), None);
        timer.add(at(100_000), ADJUSTMENT_STEP); // 15s from now, already inside the 20s warning.
        assert_eq!(timer.observe(at(100_000)), None);
        assert_eq!(timer.observe(at(115_000)), Some(TimerAlert::Finished));
    }

    #[test]
    fn subtract_never_alerts_with_a_threshold_above_the_result() {
        let mut timer = RestTimer::start_with_thresholds(
            at(0),
            secs(60),
            AlertThresholds::warning_before(secs(30)),
        );
        timer.subtract(at(10_000), secs(25)); // 25s left: inside the 30s warning.
        assert_eq!(timer.observe(at(10_000)), None);
        assert_eq!(timer.observe(at(20_000)), None);
        assert_eq!(timer.observe(at(35_000)), Some(TimerAlert::Finished));
    }

    #[test]
    fn skip_then_a_backward_clock_step_stays_silent() {
        let mut timer = rest(90);
        timer.skip(at(50_000));
        assert_eq!(
            observe_all(&mut timer, &[20_000, 49_999, 50_000, 120_000]),
            vec![]
        );
    }

    #[test]
    fn subtract_to_now_then_a_backward_clock_step_stays_silent() {
        let mut timer = rest(90);
        timer.subtract(at(80_000), ADJUSTMENT_STEP);
        assert_eq!(timer.ends_at(), at(80_000));
        assert_eq!(
            observe_all(&mut timer, &[50_000, 75_000, 80_000, 120_000]),
            vec![]
        );
    }

    #[test]
    fn finished_then_a_backward_clock_step_does_not_finish_twice() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(95_000)), Some(TimerAlert::Finished));
        assert_eq!(
            observe_all(&mut timer, &[60_000, 85_000, 90_000, 100_000]),
            vec![]
        );
    }

    #[test]
    fn a_clock_running_ahead_fires_early_but_once() {
        let mut timer = rest(90);
        // The clock was 30 minutes ahead, then corrected: the screen counts down again, silently.
        assert_eq!(timer.observe(at(1_800_000)), Some(TimerAlert::Finished));
        assert_eq!(timer.remaining(at(20_000)), secs(70));
        assert_eq!(observe_all(&mut timer, &[20_000, 80_000, 90_000]), vec![]);
    }

    #[test]
    fn skip_and_subtract_before_the_start_keep_the_start_before_the_end() {
        let mut timer = rest(90);
        timer.skip(at(-5_000));
        assert_eq!(timer.ends_at(), at(-5_000));
        assert_eq!(timer.started_at(), at(-5_000));
        assert_eq!(timer.total(), Duration::ZERO);

        let mut timer = rest(90);
        timer.subtract(at(-5_000), secs(120));
        assert_eq!(timer.ends_at(), at(-5_000));
        assert_eq!(timer.started_at(), at(-5_000));
    }

    #[test]
    fn alert_between_is_pure_and_ignores_empty_windows() {
        let timer = rest(90);
        assert_eq!(
            timer.alert_between(at(0), at(80_000)),
            Some(TimerAlert::Warning)
        );
        assert_eq!(
            timer.alert_between(at(0), at(90_000)),
            Some(TimerAlert::Finished)
        );
        assert_eq!(timer.alert_between(at(80_000), at(89_999)), None);
        assert_eq!(timer.alert_between(at(80_000), at(80_000)), None);
        assert_eq!(timer.alert_between(at(90_000), at(80_000)), None);
        assert_eq!(timer.alert_between(at(90_000), at(100_000)), None);
    }

    #[test]
    fn round_trips_through_serde_with_its_alert_state() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(81_000)), Some(TimerAlert::Warning));
        let json = serde_json::to_string(&timer).unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"started_at":{START},"ends_at":{},"warning_before_ms":10000,"announced":"warning"}}"#,
                START + 90_000
            )
        );
        let mut restored: RestTimer = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, timer);
        // A reload does not warn again.
        assert_eq!(restored.observe(at(82_000)), None);
        assert_eq!(restored.observe(at(90_000)), Some(TimerAlert::Finished));
    }

    #[test]
    fn a_reload_after_the_clock_stepped_back_still_finishes() {
        let mut timer = rest(90);
        assert_eq!(timer.observe(at(85_000)), Some(TimerAlert::Warning));
        let json = serde_json::to_string(&timer).unwrap();
        let mut restored: RestTimer = serde_json::from_str(&json).unwrap();
        // Reloaded on a clock 40 seconds behind: nothing again until the end, which still fires.
        assert_eq!(
            observe_all(&mut restored, &[45_000, 60_000, 85_000, 89_999]),
            vec![]
        );
        assert_eq!(restored.observe(at(90_000)), Some(TimerAlert::Finished));
    }

    #[test]
    fn deserialization_validates_the_timer() {
        let parse = |json: &str| serde_json::from_str::<RestTimer>(json);
        let ok =
            r#"{"started_at":0,"ends_at":90000,"warning_before_ms":null,"announced":"nothing"}"#;
        assert_eq!(
            parse(ok).unwrap().ends_at(),
            Timestamp::from_epoch_millis(90_000)
        );
        assert!(parse(ok).unwrap().thresholds().warning().is_none());
        let backwards = r#"{"started_at":90001,"ends_at":90000,"warning_before_ms":10000,"announced":"nothing"}"#;
        assert!(
            parse(backwards)
                .unwrap_err()
                .to_string()
                .contains("cannot end before it starts")
        );
        let unknown =
            r#"{"started_at":0,"ends_at":90000,"warning_before_ms":10000,"announced":"maybe"}"#;
        assert!(parse(unknown).is_err());
    }
}
