use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{AlertThresholds, Countdown, TimerAlert};
use crate::time::Timestamp;

/// A timed hold (a plank, a dead hang): a countdown towards a target, with the time held so far.
///
/// Alerts follow the same rules as the rest timer (see the [module documentation](super)):
/// [`TimerAlert::Warning`] before the target, [`TimerAlert::Finished`] when it is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HoldTimer(Countdown);

impl HoldTimer {
    /// Starts a hold towards `target` at `now`, warning 10 seconds before the target.
    #[must_use]
    pub fn start(now: Timestamp, target: Duration) -> Self {
        Self::start_with_thresholds(now, target, AlertThresholds::default())
    }

    /// Starts a hold towards `target` at `now` with custom alert thresholds.
    #[must_use]
    pub fn start_with_thresholds(
        now: Timestamp,
        target: Duration,
        thresholds: AlertThresholds,
    ) -> Self {
        Self(Countdown::start(now, target, thresholds))
    }

    /// When the hold started.
    #[must_use]
    pub fn started_at(&self) -> Timestamp {
        self.0.started_at
    }

    /// When the target is reached.
    #[must_use]
    pub fn ends_at(&self) -> Timestamp {
        self.0.ends_at
    }

    /// The target hold time, to the millisecond.
    #[must_use]
    pub fn target(&self) -> Duration {
        self.0.total()
    }

    /// The thresholds this hold alerts at.
    #[must_use]
    pub fn thresholds(&self) -> AlertThresholds {
        self.0.thresholds()
    }

    /// The latest instant alerts were computed up to.
    #[must_use]
    pub fn observed_until(&self) -> Timestamp {
        self.0.observed_until
    }

    /// Time held at `now`, not capped at the target (holding longer counts). Zero before the start.
    #[must_use]
    pub fn elapsed(&self, now: Timestamp) -> Duration {
        now.saturating_duration_since(self.0.started_at)
    }

    /// Time left until the target at `now`, zero once it is reached.
    #[must_use]
    pub fn remaining(&self, now: Timestamp) -> Duration {
        self.0.remaining(now)
    }

    /// Whether the target is reached at `now`.
    #[must_use]
    pub fn is_finished(&self, now: Timestamp) -> bool {
        self.0.is_finished(now)
    }

    /// The alert crossed between `previous` (excluded) and `now` (included), if any. Pure: prefer
    /// [`observe`](Self::observe), which remembers `previous`.
    #[must_use]
    pub fn alert_between(&self, previous: Timestamp, now: Timestamp) -> Option<TimerAlert> {
        self.0.alert_between(previous, now)
    }

    /// The alert to raise now, if any, given everything observed so far.
    pub fn observe(&mut self, now: Timestamp) -> Option<TimerAlert> {
        self.0.observe(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset_ms: i64) -> Timestamp {
        Timestamp::from_epoch_millis(1_700_000_000_000 + offset_ms)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn tracks_elapsed_and_remaining_time() {
        let hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(hold.started_at(), at(0));
        assert_eq!(hold.ends_at(), at(60_000));
        assert_eq!(hold.target(), secs(60));
        assert_eq!(hold.thresholds(), AlertThresholds::default());
        assert_eq!(hold.observed_until(), at(0));
        assert_eq!(hold.elapsed(at(0)), Duration::ZERO);
        assert_eq!(hold.remaining(at(0)), secs(60));
        assert_eq!(hold.elapsed(at(25_000)), secs(25));
        assert_eq!(hold.remaining(at(25_000)), secs(35));
        assert!(!hold.is_finished(at(59_999)));
        assert!(hold.is_finished(at(60_000)));
    }

    #[test]
    fn elapsed_keeps_counting_past_the_target() {
        let hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(hold.elapsed(at(75_000)), secs(75));
        assert_eq!(hold.remaining(at(75_000)), Duration::ZERO);
    }

    #[test]
    fn elapsed_is_zero_before_the_start() {
        let hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(hold.elapsed(at(-3_000)), Duration::ZERO);
        assert_eq!(hold.remaining(at(-3_000)), secs(63));
    }

    #[test]
    fn warns_then_finishes_once() {
        let mut hold = HoldTimer::start(at(0), secs(60));
        let alerts: Vec<(i64, TimerAlert)> =
            [0, 49_999, 50_000, 50_000, 59_999, 60_000, 60_000, 90_000]
                .into_iter()
                .filter_map(|t| hold.observe(at(t)).map(|alert| (t, alert)))
                .collect();
        assert_eq!(
            alerts,
            vec![
                (50_000, TimerAlert::Warning),
                (60_000, TimerAlert::Finished)
            ]
        );
    }

    #[test]
    fn a_gap_over_the_target_only_finishes() {
        let mut hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(hold.observe(at(5_000)), None);
        assert_eq!(hold.observe(at(120_000)), Some(TimerAlert::Finished));
        assert_eq!(hold.observe(at(10_000)), None);
        assert_eq!(hold.observe(at(130_000)), None);
    }

    #[test]
    fn thresholds_are_configurable() {
        let mut hold =
            HoldTimer::start_with_thresholds(at(0), secs(30), AlertThresholds::without_warning());
        assert_eq!(hold.observe(at(25_000)), None);
        assert_eq!(hold.observe(at(30_000)), Some(TimerAlert::Finished));
    }

    #[test]
    fn alert_between_is_pure() {
        let hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(
            hold.alert_between(at(0), at(50_000)),
            Some(TimerAlert::Warning)
        );
        assert_eq!(
            hold.alert_between(at(0), at(60_000)),
            Some(TimerAlert::Finished)
        );
        assert_eq!(hold.alert_between(at(60_000), at(0)), None);
        assert_eq!(hold.observed_until(), at(0));
    }

    #[test]
    fn round_trips_through_serde() {
        let mut hold = HoldTimer::start(at(0), secs(60));
        assert_eq!(hold.observe(at(55_000)), Some(TimerAlert::Warning));
        let json = serde_json::to_string(&hold).unwrap();
        let mut restored: HoldTimer = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, hold);
        assert_eq!(restored.observe(at(56_000)), None);
    }
}
