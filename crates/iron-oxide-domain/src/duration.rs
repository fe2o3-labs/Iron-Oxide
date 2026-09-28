//! Whole-second durations: rest periods, holds, intervals.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Quantity, ValueError};

/// A non-negative duration in whole seconds (rest time, plank hold, interval length).
///
/// Serializes as a plain integer number of seconds. Displays as a clock: `1:30`, `0:05`, `1:02:03`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Seconds(u32);

impl Seconds {
    /// No time.
    pub const ZERO: Self = Self(0);

    /// Wraps a number of seconds. Every `u32` is valid.
    #[must_use]
    pub const fn new(seconds: u32) -> Self {
        Self(seconds)
    }

    /// Builds a duration from whole minutes.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] when the result does not fit.
    pub const fn from_minutes(minutes: u32) -> Result<Self, ValueError> {
        match minutes.checked_mul(60) {
            Some(seconds) => Ok(Self(seconds)),
            None => Err(too_large()),
        }
    }

    /// Converts milliseconds, rounding **up** so that a countdown showing the result never reaches
    /// `0:00` before the time is really over (1 ms left shows as `0:01`).
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] when the result does not fit.
    pub fn from_millis_ceil(millis: u64) -> Result<Self, ValueError> {
        u32::try_from(millis.div_ceil(1_000))
            .map(Self)
            .map_err(|_| too_large())
    }

    /// The number of seconds.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The duration in milliseconds, e.g. to add to an epoch-millisecond timestamp. Cannot overflow.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        // u32::MAX * 1000 < u64::MAX
        self.0 as u64 * 1_000
    }

    /// The equivalent [`std::time::Duration`].
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        Duration::from_secs(self.0 as u64)
    }

    /// Whether this is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Sum.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] on overflow.
    pub const fn checked_add(self, rhs: Self) -> Result<Self, ValueError> {
        match self.0.checked_add(rhs.0) {
            Some(seconds) => Ok(Self(seconds)),
            None => Err(too_large()),
        }
    }

    /// Product with a count, e.g. an interval length times the number of rounds.
    ///
    /// # Errors
    /// [`ValueError::TooLarge`] on overflow.
    pub const fn checked_mul(self, count: u32) -> Result<Self, ValueError> {
        match self.0.checked_mul(count) {
            Some(seconds) => Ok(Self(seconds)),
            None => Err(too_large()),
        }
    }

    /// Difference clamped at zero.
    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

const fn too_large() -> ValueError {
    ValueError::TooLarge {
        quantity: Quantity::Seconds,
        max: "4294967295 s",
    }
}

impl From<u32> for Seconds {
    fn from(seconds: u32) -> Self {
        Self(seconds)
    }
}

impl From<Seconds> for u32 {
    fn from(seconds: Seconds) -> Self {
        seconds.0
    }
}

impl From<Seconds> for Duration {
    fn from(seconds: Seconds) -> Self {
        seconds.as_duration()
    }
}

impl TryFrom<i64> for Seconds {
    type Error = ValueError;

    /// Converts from a wider signed integer (form input, database column).
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            #[allow(clippy::cast_precision_loss)] // only used for the error message
            return Err(ValueError::Negative {
                quantity: Quantity::Seconds,
                value: value as f64,
            });
        }
        u32::try_from(value).map(Self).map_err(|_| too_large())
    }
}

impl fmt::Display for Seconds {
    /// `m:ss` under an hour, `h:mm:ss` from one hour.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hours = self.0 / 3_600;
        let minutes = self.0 % 3_600 / 60;
        let seconds = self.0 % 60;
        if hours == 0 {
            write!(f, "{minutes}:{seconds:02}")
        } else {
            write!(f, "{hours}:{minutes:02}:{seconds:02}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_and_accessors() {
        let rest = Seconds::new(90);
        assert_eq!(rest.get(), 90);
        assert_eq!(u32::from(rest), 90);
        assert_eq!(Seconds::from(90_u32), rest);
        assert_eq!(Seconds::from_minutes(3).unwrap(), Seconds::new(180));
        assert!(Seconds::from_minutes(u32::MAX / 60 + 1).is_err());
        assert_eq!(
            Seconds::from_minutes(u32::MAX / 60).unwrap().get(),
            u32::MAX / 60 * 60
        );
        assert_eq!(rest.as_millis(), 90_000);
        assert_eq!(
            Seconds::new(u32::MAX).as_millis(),
            u64::from(u32::MAX) * 1_000
        );
        assert_eq!(rest.as_duration(), Duration::from_secs(90));
        assert_eq!(Duration::from(rest), Duration::from_secs(90));
        assert!(Seconds::ZERO.is_zero());
        assert!(!rest.is_zero());
        assert_eq!(Seconds::default(), Seconds::ZERO);
    }

    #[test]
    fn from_millis_rounds_up() {
        assert_eq!(Seconds::from_millis_ceil(0).unwrap(), Seconds::ZERO);
        assert_eq!(Seconds::from_millis_ceil(1).unwrap(), Seconds::new(1));
        assert_eq!(Seconds::from_millis_ceil(1_000).unwrap(), Seconds::new(1));
        assert_eq!(Seconds::from_millis_ceil(1_001).unwrap(), Seconds::new(2));
        let max_millis = u64::from(u32::MAX) * 1_000;
        assert_eq!(
            Seconds::from_millis_ceil(max_millis).unwrap(),
            Seconds::new(u32::MAX)
        );
        assert!(Seconds::from_millis_ceil(max_millis + 1).is_err());
        assert!(Seconds::from_millis_ceil(u64::MAX).is_err());
    }

    #[test]
    fn arithmetic() {
        assert_eq!(
            Seconds::new(60).checked_add(Seconds::new(15)).unwrap(),
            Seconds::new(75)
        );
        assert!(Seconds::new(u32::MAX).checked_add(Seconds::new(1)).is_err());
        assert_eq!(Seconds::new(30).checked_mul(8).unwrap(), Seconds::new(240));
        assert!(Seconds::new(u32::MAX).checked_mul(2).is_err());
        assert_eq!(
            Seconds::new(10).saturating_sub(Seconds::new(15)),
            Seconds::ZERO
        );
        assert_eq!(
            Seconds::new(90).saturating_sub(Seconds::new(15)),
            Seconds::new(75)
        );
    }

    #[test]
    fn try_from_i64_boundaries() {
        assert_eq!(Seconds::try_from(0_i64).unwrap(), Seconds::ZERO);
        assert_eq!(
            Seconds::try_from(i64::from(u32::MAX)).unwrap(),
            Seconds::new(u32::MAX)
        );
        assert_eq!(
            Seconds::try_from(i64::from(u32::MAX) + 1)
                .unwrap_err()
                .to_string(),
            "duration must be at most 4294967295 s"
        );
        assert_eq!(
            Seconds::try_from(-5_i64).unwrap_err().to_string(),
            "duration must not be negative (got -5)"
        );
    }

    #[test]
    fn displays_as_a_clock() {
        assert_eq!(Seconds::ZERO.to_string(), "0:00");
        assert_eq!(Seconds::new(5).to_string(), "0:05");
        assert_eq!(Seconds::new(90).to_string(), "1:30");
        assert_eq!(Seconds::new(3_599).to_string(), "59:59");
        assert_eq!(Seconds::new(3_600).to_string(), "1:00:00");
        assert_eq!(Seconds::new(3_723).to_string(), "1:02:03");
    }

    #[test]
    fn serde_is_a_bare_integer() {
        assert_eq!(serde_json::to_string(&Seconds::new(90)).unwrap(), "90");
        assert_eq!(
            serde_json::from_str::<Seconds>("90").unwrap(),
            Seconds::new(90)
        );
        assert!(serde_json::from_str::<Seconds>("-1").is_err());
        assert!(serde_json::from_str::<Seconds>("1.5").is_err());
    }
}
