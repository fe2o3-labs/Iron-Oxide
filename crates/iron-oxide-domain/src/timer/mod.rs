//! Timer maths for rests and timed work, derived from timestamps.
//!
//! Timers never count ticks. They store the instants that matter (when they started, when they
//! end) and derive everything else from the current time, which the caller passes in. The UI can
//! therefore be suspended for minutes (screen locked, tab in the background) and still show the
//! right value as soon as it looks again.
//!
//! # Alerts and events
//!
//! Each start or adjustment of a countdown *arms* it. Every timer remembers how far the current
//! arming has been announced (nothing, the warning, or the finish; for intervals, the latest
//! phase), and [`RestTimer::observe`] and friends only need `now`: an alert fires when the level
//! reached at `now` is past what was already announced.
//!
//! - **Exactly once per arming.** Announcements only move forward, so each alert fires at most
//!   once, however often or in whatever order the timer is observed.
//! - **Gaps collapse to the latest alert.** If the screen was locked through several thresholds,
//!   only the most advanced one fires: a rest that is over reports [`TimerAlert::Finished`] alone,
//!   never a stale "10 seconds left" right before it. A gap over the warning only warns late.
//! - **User actions never alert.** Starting and adjusting a timer (`+15s`, `-15s`, skip) arm it
//!   at the action's own `now`, marking whatever is already reached as announced, silently: a rest
//!   of 10 seconds or less does not warn, `-15s` over the warning stays silent, ending the rest with
//!   `-15s` or skip stays silent, and `+15s` back above a threshold re-arms it.
//! - **Clock skew.** The state records what was announced, never when the timer was last looked
//!   at, so no instant can be "ahead" of the clock and silence the timer. After the clock steps
//!   back (or a reload on a device whose clock differs), alerts already announced for the current
//!   arming stay announced and never fire twice, and the others still fire once their threshold is
//!   reached. A clock that runs ahead fires an alert early, never twice: once it is corrected the
//!   screen counts down again, without a second beep.
//!
//! The pure [`RestTimer::alert_between`] and [`IntervalTimer::event_between`] give the alert
//! crossed between two instants for the current arming, ignoring what was announced.

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

/// How far the current arming of a countdown has been announced, in the order alerts fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Announced {
    Nothing,
    Warning,
    Finished,
}

/// Why a persisted countdown is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a timer cannot end before it starts")]
struct EndsBeforeStart;

/// A countdown towards `ends_at`, with alert bookkeeping. Shared by the rest and hold timers.
///
/// Invariant: `started_at <= ends_at`, checked on deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "CountdownRepr")]
struct Countdown {
    started_at: Timestamp,
    ends_at: Timestamp,
    /// The warning lead time in milliseconds, if the warning is enabled.
    warning_before_ms: Option<u64>,
    /// How far the current arming has been announced.
    announced: Announced,
}

/// The serialized shape of a [`Countdown`], validated on the way in.
#[derive(Deserialize)]
struct CountdownRepr {
    started_at: Timestamp,
    ends_at: Timestamp,
    warning_before_ms: Option<u64>,
    announced: Announced,
}

impl TryFrom<CountdownRepr> for Countdown {
    type Error = EndsBeforeStart;

    fn try_from(repr: CountdownRepr) -> Result<Self, Self::Error> {
        if repr.ends_at < repr.started_at {
            return Err(EndsBeforeStart);
        }
        Ok(Self {
            started_at: repr.started_at,
            ends_at: repr.ends_at,
            warning_before_ms: repr.warning_before_ms,
            announced: repr.announced,
        })
    }
}

impl Countdown {
    fn start(now: Timestamp, duration: Duration, thresholds: AlertThresholds) -> Self {
        let mut countdown = Self {
            started_at: now,
            ends_at: now.saturating_add(duration),
            warning_before_ms: thresholds.warning_before.map(duration_to_millis),
            announced: Announced::Nothing,
        };
        countdown.arm(now);
        countdown
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

    /// The most advanced alert level reached at `at`.
    fn level(&self, at: Timestamp) -> Announced {
        let remaining = self.remaining_ms(at);
        if remaining == 0 {
            Announced::Finished
        } else if self
            .warning_before_ms
            .is_some_and(|warning| remaining <= warning)
        {
            Announced::Warning
        } else {
            Announced::Nothing
        }
    }

    fn alert_between(&self, previous: Timestamp, now: Timestamp) -> Option<TimerAlert> {
        if now <= previous {
            return None;
        }
        Self::alert_for(self.level(previous), self.level(now))
    }

    fn observe(&mut self, now: Timestamp) -> Option<TimerAlert> {
        let reached = self.level(now);
        let alert = Self::alert_for(self.announced, reached);
        self.announced = self.announced.max(reached);
        alert
    }

    /// Starts a new arming at `now`: whatever is already reached counts as announced, silently.
    fn arm(&mut self, now: Timestamp) {
        self.announced = self.level(now);
    }

    /// Keeps `started_at <= ends_at` after the end moved earlier (possible under clock skew).
    fn clamp_start(&mut self) {
        self.started_at = self.started_at.min(self.ends_at);
    }

    fn alert_for(announced: Announced, reached: Announced) -> Option<TimerAlert> {
        if reached <= announced {
            return None;
        }
        match reached {
            Announced::Nothing => None,
            Announced::Warning => Some(TimerAlert::Warning),
            Announced::Finished => Some(TimerAlert::Finished),
        }
    }
}
