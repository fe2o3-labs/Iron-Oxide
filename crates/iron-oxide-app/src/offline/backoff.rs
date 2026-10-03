//! When to retry a write that failed with a retryable error: exponential backoff with full
//! jitter, capped, and never sooner than a `429`'s `Retry-After`.
//!
//! Pure: the caller passes the random draw, so the schedule is testable.

use std::time::Duration;

/// An exponential backoff schedule with full jitter.
///
/// After the `n`-th consecutive failure (from 1), the ceiling is `min(cap, base × 2^(n-1))` and
/// the delay a uniform draw in `[0, ceiling]` ("full jitter"): clients that lost the connection
/// together do not come back together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub cap: Duration,
}

impl Backoff {
    /// The outbox's schedule: 1 s, 2 s, 4 s, … up to 5 minutes.
    pub const DEFAULT: Self = Self {
        base: Duration::from_secs(1),
        cap: Duration::from_secs(5 * 60),
    };

    /// The largest delay after the `failures`-th consecutive failure. Saturates instead of
    /// overflowing, however many failures.
    #[must_use]
    pub fn ceiling(&self, failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(31);
        self.base
            .checked_mul(1 << exponent)
            .map_or(self.cap, |delay| delay.min(self.cap))
    }

    /// The delay after the `failures`-th consecutive failure. `random` is a uniform draw in
    /// `[0, 1)` (`Math.random()`); anything outside `[0, 1]` (or NaN) is clamped.
    #[must_use]
    pub fn delay(&self, failures: u32, random: f64) -> Duration {
        let random = if random.is_nan() {
            1.0
        } else {
            random.clamp(0.0, 1.0)
        };
        self.ceiling(failures).mul_f64(random)
    }

    /// The delay before the next attempt: the jittered backoff, but never less than the server's
    /// `Retry-After` when it sent one (a `429` is never retried sooner).
    #[must_use]
    pub fn next_delay(
        &self,
        failures: u32,
        random: f64,
        retry_after: Option<Duration>,
    ) -> Duration {
        let delay = self.delay(failures, random);
        retry_after.map_or(delay, |retry_after| delay.max(retry_after))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Backoff = Backoff::DEFAULT;

    #[test]
    fn ceiling_doubles_from_the_base_up_to_the_cap() {
        assert_eq!(B.ceiling(0), Duration::from_secs(1));
        assert_eq!(B.ceiling(1), Duration::from_secs(1));
        assert_eq!(B.ceiling(2), Duration::from_secs(2));
        assert_eq!(B.ceiling(3), Duration::from_secs(4));
        assert_eq!(B.ceiling(9), Duration::from_secs(256));
        assert_eq!(B.ceiling(10), B.cap);
        assert_eq!(B.ceiling(1_000), B.cap);
        assert_eq!(B.ceiling(u32::MAX), B.cap);
    }

    #[test]
    fn full_jitter_stays_within_zero_and_the_ceiling() {
        for failures in [1, 2, 5, 12, u32::MAX] {
            for step in 0..=100 {
                let random = f64::from(step) / 100.0;
                let delay = B.delay(failures, random);
                assert!(delay <= B.ceiling(failures), "{failures} {random}");
            }
            assert_eq!(B.delay(failures, 0.0), Duration::ZERO);
            assert_eq!(B.delay(failures, 1.0), B.ceiling(failures));
        }
        assert_eq!(B.delay(3, 0.5), Duration::from_secs(2));
    }

    #[test]
    fn out_of_range_random_draws_are_clamped() {
        assert_eq!(B.delay(3, -1.0), Duration::ZERO);
        assert_eq!(B.delay(3, 7.0), Duration::from_secs(4));
        assert_eq!(B.delay(3, f64::NAN), Duration::from_secs(4));
        assert_eq!(B.delay(3, f64::INFINITY), Duration::from_secs(4));
    }

    #[test]
    fn retry_after_takes_precedence_over_a_shorter_backoff() {
        let thirty = Duration::from_secs(30);
        assert_eq!(B.next_delay(1, 0.9, Some(thirty)), thirty);
        // A longer backoff is kept: Retry-After is a minimum.
        assert_eq!(
            B.next_delay(12, 1.0, Some(thirty)),
            Duration::from_secs(300)
        );
        assert_eq!(B.next_delay(2, 0.5, None), Duration::from_secs(1));
        // Retry-After is never capped by the backoff's cap.
        let hour = Duration::from_secs(3_600);
        assert_eq!(B.next_delay(1, 0.0, Some(hour)), hour);
    }
}
