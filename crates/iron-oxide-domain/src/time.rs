//! Points in time for the domain.
//!
//! The domain never reads the system clock. Callers (the UI, the server) read their own clock
//! and pass the current [`Timestamp`] in, which keeps every rule deterministic and testable.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A point in time: milliseconds since the Unix epoch, UTC.
///
/// Arithmetic with [`Duration`] saturates at the bounds of `i64` instead of overflowing, and
/// sub-millisecond parts of a duration are truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch, `1970-01-01T00:00:00Z`.
    pub const EPOCH: Self = Self(0);

    /// Builds a timestamp from milliseconds since the Unix epoch (negative is before 1970).
    #[must_use]
    pub const fn from_epoch_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn epoch_millis(self) -> i64 {
        self.0
    }

    /// `self + duration`, saturating at the latest representable timestamp.
    #[must_use]
    pub fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add_unsigned(duration_to_millis(duration)))
    }

    /// `self - duration`, saturating at the earliest representable timestamp.
    #[must_use]
    pub fn saturating_sub(self, duration: Duration) -> Self {
        Self(self.0.saturating_sub_unsigned(duration_to_millis(duration)))
    }

    /// Time elapsed from `earlier` to `self`, or zero if `earlier` is not before `self`.
    #[must_use]
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_millis(self.saturating_millis_since(earlier))
    }

    /// Milliseconds elapsed from `earlier` to `self`, or zero if `earlier` is not before `self`.
    pub(crate) fn saturating_millis_since(self, earlier: Self) -> u64 {
        // The difference of two `i64` always fits in a `u64` when it is positive.
        u64::try_from(i128::from(self.0) - i128::from(earlier.0)).unwrap_or(0)
    }

    /// `self + millis`, saturating at the latest representable timestamp.
    pub(crate) fn saturating_add_millis(self, millis: u64) -> Self {
        Self(self.0.saturating_add_unsigned(millis))
    }
}

/// Whole milliseconds in `duration`, truncating sub-millisecond parts and saturating at `u64::MAX`.
pub(crate) fn duration_to_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_epoch_millis(millis)
    }

    #[test]
    fn round_trips_epoch_millis() {
        assert_eq!(ts(1_700_000_000_123).epoch_millis(), 1_700_000_000_123);
        assert_eq!(ts(-5).epoch_millis(), -5);
        assert_eq!(Timestamp::EPOCH.epoch_millis(), 0);
    }

    #[test]
    fn orders_chronologically() {
        assert!(ts(-1) < Timestamp::EPOCH);
        assert!(ts(1) > Timestamp::EPOCH);
    }

    #[test]
    fn adds_and_subtracts_durations() {
        assert_eq!(
            ts(1_000).saturating_add(Duration::from_secs(15)),
            ts(16_000)
        );
        assert_eq!(
            ts(1_000).saturating_sub(Duration::from_secs(15)),
            ts(-14_000)
        );
    }

    #[test]
    fn truncates_sub_millisecond_parts() {
        assert_eq!(ts(0).saturating_add(Duration::from_micros(1_999)), ts(1));
        assert_eq!(ts(0).saturating_add(Duration::from_nanos(999_999)), ts(0));
    }

    #[test]
    fn saturates_at_the_bounds() {
        assert_eq!(
            ts(i64::MAX - 1).saturating_add(Duration::from_millis(5)),
            ts(i64::MAX)
        );
        assert_eq!(ts(0).saturating_add(Duration::MAX), ts(i64::MAX));
        assert_eq!(
            ts(i64::MIN + 1).saturating_sub(Duration::from_millis(5)),
            ts(i64::MIN)
        );
        assert_eq!(ts(0).saturating_sub(Duration::MAX), ts(i64::MIN));
        assert_eq!(ts(i64::MAX).saturating_add_millis(u64::MAX), ts(i64::MAX));
        assert_eq!(ts(i64::MIN).saturating_add_millis(u64::MAX), ts(i64::MAX));
    }

    #[test]
    fn measures_elapsed_time_saturating_at_zero() {
        assert_eq!(
            ts(5_500).saturating_duration_since(ts(1_000)),
            Duration::from_millis(4_500)
        );
        assert_eq!(
            ts(1_000).saturating_duration_since(ts(1_000)),
            Duration::ZERO
        );
        assert_eq!(
            ts(1_000).saturating_duration_since(ts(5_500)),
            Duration::ZERO
        );
        assert_eq!(ts(i64::MAX).saturating_millis_since(ts(i64::MIN)), u64::MAX);
        assert_eq!(ts(i64::MIN).saturating_millis_since(ts(i64::MAX)), 0);
    }

    #[test]
    fn converts_durations_to_millis() {
        assert_eq!(duration_to_millis(Duration::ZERO), 0);
        assert_eq!(duration_to_millis(Duration::from_secs(15)), 15_000);
        assert_eq!(duration_to_millis(Duration::MAX), u64::MAX);
    }

    #[test]
    fn serializes_as_a_bare_integer() {
        assert_eq!(
            serde_json::to_string(&ts(1_700_000_000_123)).unwrap(),
            "1700000000123"
        );
        assert_eq!(serde_json::from_str::<Timestamp>("-42").unwrap(), ts(-42));
    }
}
