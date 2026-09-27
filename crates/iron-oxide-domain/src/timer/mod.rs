//! Timer maths for rests and timed work, derived from timestamps.
//!
//! Timers never count ticks. They store the instants that matter (when they started, when they
//! end) and derive everything else from the current time, which the caller passes in. The UI can
//! therefore be suspended for minutes (screen locked, tab in the background) and still show the
//! right value as soon as it looks again.
//!
//! # Alerts and events
//!
//! Alerts are computed from the window `(previous observation, now]`: an alert fires when its
//! threshold is crossed inside that window. Each timer remembers the latest instant it was
//! observed at (`observed_until`), so [`RestTimer::observe`] and friends only need `now`:
//!
//! - **Exactly once.** For a given end time the remaining time only decreases as the observation
//!   window moves forward, so each threshold is crossed at most once. Observing twice at the same
//!   instant yields nothing the second time.
//! - **Gaps collapse to the latest alert.** If one window crosses several thresholds (the screen
//!   was locked for five minutes), only the most advanced one fires: a rest that is over reports
//!   [`TimerAlert::Finished`] alone, never a stale "10 seconds left" right before it.
//! - **Clock skew is ignored.** `observed_until` never moves backwards. An observation earlier
//!   than a previous one yields nothing, and the later forward jump does not cross again what was
//!   already crossed.
//! - **User actions never alert.** Adjusting a timer (`+15s`, `-15s`, skip) moves the observation
//!   mark to the adjustment instant, so alerts only come from time passing. Jumping over the
//!   warning with `-15s` stays silent, ending the rest with `-15s` or skip stays silent, and
//!   `+15s` back above a threshold re-arms it.

mod hold;
mod interval;
mod rest;

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use hold::HoldTimer;
pub use interval::{
    IntervalEvent, IntervalPhase, IntervalPlan, IntervalPlanError, IntervalStatus, IntervalTimer,
};
pub use rest::RestTimer;

use crate::time::{Timestamp, duration_to_millis};

/// The step used by the `+15s` / `-15s` buttons of the rest timer.
pub const ADJUSTMENT_STEP: Duration = Duration::from_secs(15);

/// An alert raised by a countdown when a threshold is crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimerAlert {
    /// The remaining time dropped to the warning threshold (10 seconds by default).
    Warning,
    /// The remaining time reached zero.
    Finished,
}

/// When a countdown raises its alerts. [`TimerAlert::Finished`] always fires at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlertThresholds {
    warning_before: Option<Duration>,
}

impl AlertThresholds {
    /// The default warning lead time: 10 seconds before the end.
    pub const DEFAULT_WARNING: Duration = Duration::from_secs(10);

    /// Warns when `before` or less is left. A zero lead time never warns, since it coincides with
    /// [`TimerAlert::Finished`], which wins.
    #[must_use]
    pub const fn warning_before(before: Duration) -> Self {
        Self {
            warning_before: Some(before),
        }
    }

    /// Only [`TimerAlert::Finished`] fires.
    #[must_use]
    pub const fn without_warning() -> Self {
        Self {
            warning_before: None,
        }
    }

    /// The warning lead time, if the warning is enabled.
    #[must_use]
    pub const fn warning(self) -> Option<Duration> {
        self.warning_before
    }
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self::warning_before(Self::DEFAULT_WARNING)
    }
}

/// A countdown towards `ends_at`, with alert bookkeeping. Shared by the rest and hold timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Countdown {
    started_at: Timestamp,
    ends_at: Timestamp,
    /// The latest instant alerts were computed up to. Never moves backwards.
    observed_until: Timestamp,
    /// The warning lead time in milliseconds, if the warning is enabled.
    warning_before_ms: Option<u64>,
}

impl Countdown {
    fn start(now: Timestamp, duration: Duration, thresholds: AlertThresholds) -> Self {
        Self {
            started_at: now,
            ends_at: now.saturating_add(duration),
            observed_until: now,
            warning_before_ms: thresholds.warning_before.map(duration_to_millis),
        }
    }

    fn thresholds(&self) -> AlertThresholds {
        AlertThresholds {
            warning_before: self.warning_before_ms.map(Duration::from_millis),
        }
    }

    fn remaining_ms(&self, at: Timestamp) -> u64 {
        self.ends_at.saturating_millis_since(at)
    }

    fn remaining(&self, at: Timestamp) -> Duration {
        Duration::from_millis(self.remaining_ms(at))
    }

    fn total(&self) -> Duration {
        self.ends_at.saturating_duration_since(self.started_at)
    }

    fn is_finished(&self, at: Timestamp) -> bool {
        at >= self.ends_at
    }

    fn alert_between(&self, previous: Timestamp, now: Timestamp) -> Option<TimerAlert> {
        if now <= previous {
            return None;
        }
        let before = self.remaining_ms(previous);
        let after = self.remaining_ms(now);
        if before > 0 && after == 0 {
            return Some(TimerAlert::Finished);
        }
        match self.warning_before_ms {
            Some(warning) if before > warning && after <= warning => Some(TimerAlert::Warning),
            _ => None,
        }
    }

    fn observe(&mut self, now: Timestamp) -> Option<TimerAlert> {
        let previous = self.observed_until;
        self.settle(now);
        self.alert_between(previous, now)
    }

    /// Moves the observation mark to `now` (never backwards) without raising alerts.
    fn settle(&mut self, now: Timestamp) {
        self.observed_until = self.observed_until.max(now);
    }
}
