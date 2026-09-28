//! A keyed token bucket (GCRA) with a hard cap on the number of keys it remembers.
//!
//! Each key holds one instant, its "theoretical arrival time" (TAT): the moment its bucket will
//! be full again. A request at `now` is allowed when `max(TAT, now) + period` is at most
//! `now + burst × period`, and then moves the TAT to that value. A refused request changes
//! nothing, so hammering a limit does not push it further away.
//!
//! A key whose TAT is in the past has a full bucket, exactly like a key never seen: forgetting it
//! loses nothing. Memory is bounded by count, not by time: when a new key arrives and the table
//! holds [`KeyedLimiter::capacity`] keys, the full-bucket keys are dropped first, then (if that
//! freed too little) the keys closest to a full bucket. Keys that are being limited are the last
//! to go. Each such sweep frees at least an eighth of the table, so its cost is amortised over
//! many inserts and a flood of new keys cannot turn it into a CPU sink.

use std::{
    collections::HashMap,
    hash::Hash,
    num::NonZeroU32,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use tokio::time::Instant;

/// How many requests a key may make: `burst` at once, then one more every `period`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    burst: NonZeroU32,
    period: Duration,
}

impl Quota {
    /// `burst` requests at once, then one every `period`. `None` when `period` is zero.
    #[must_use]
    pub const fn new(burst: NonZeroU32, period: Duration) -> Option<Self> {
        if period.is_zero() {
            None
        } else {
            Some(Self { burst, period })
        }
    }

    /// `burst` requests at once, refilled at `burst` per `window` (e.g. 30 per minute). For
    /// constants: an invalid quota (a zero burst, or a window too short to split into `burst`
    /// periods) fails the build.
    #[allow(
        clippy::panic,
        reason = "only evaluated in constants, where it is a build error"
    )]
    #[must_use]
    pub const fn per(burst: u32, window: Duration) -> Self {
        let Some(nonzero) = NonZeroU32::new(burst) else {
            panic!("a quota needs a burst of at least 1");
        };
        let period = match window.checked_div(burst) {
            Some(period) => period,
            None => Duration::ZERO,
        };
        match Self::new(nonzero, period) {
            Some(quota) => quota,
            None => panic!("a quota's window is too short for its burst"),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn burst(self) -> u32 {
        self.burst.get()
    }

    /// The time to earn one request back.
    #[cfg(test)]
    #[must_use]
    pub const fn period(self) -> Duration {
        self.period
    }

    /// How far the TAT may run ahead of now: the whole burst.
    fn tolerance(self) -> Duration {
        self.period.saturating_mul(self.burst.get())
    }
}

/// A refused request: how long until the key may make one again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limited {
    pub retry_after: Duration,
}

/// The default number of keys a limiter remembers.
pub const DEFAULT_CAPACITY: usize = 50_000;

/// A per-key rate limit. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct KeyedLimiter<K> {
    quota: Quota,
    capacity: usize,
    tats: Mutex<HashMap<K, Instant>>,
}

impl<K: Hash + Eq + Copy> KeyedLimiter<K> {
    /// A limiter remembering at most `capacity` keys (at least 1).
    #[must_use]
    pub fn new(quota: Quota, capacity: usize) -> Self {
        Self {
            quota,
            capacity: capacity.max(1),
            tats: Mutex::new(HashMap::new()),
        }
    }

    /// The most keys it remembers.
    #[cfg(test)]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many keys it remembers now.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Counts one request from `key` at `now`.
    pub fn check(&self, key: K, now: Instant) -> Result<(), Limited> {
        let quota = self.quota;
        let mut tats = self.lock();
        let tat = tats.get(&key).copied().unwrap_or(now).max(now);
        let next = tat + quota.period;
        let ahead = next.saturating_duration_since(now);
        if ahead > quota.tolerance() {
            return Err(Limited {
                retry_after: ahead - quota.tolerance(),
            });
        }
        if !tats.contains_key(&key) && tats.len() >= self.capacity {
            Self::make_room(&mut tats, self.capacity, now);
        }
        tats.insert(key, next);
        Ok(())
    }

    /// Frees at least an eighth of the table (and at least one slot).
    fn make_room(tats: &mut HashMap<K, Instant>, capacity: usize, now: Instant) {
        // Full buckets carry no state.
        tats.retain(|_, tat| *tat > now);
        let target = capacity - (capacity / 8).max(1);
        if tats.len() <= target {
            return;
        }
        let excess = tats.len() - target;
        let mut by_tat: Vec<(K, Instant)> = tats.iter().map(|(key, tat)| (*key, *tat)).collect();
        // The `excess` keys closest to a full bucket, in no particular order.
        by_tat.select_nth_unstable_by_key(excess - 1, |(_, tat)| *tat);
        for (key, _) in &by_tat[..excess] {
            tats.remove(key);
        }
        dioxus::logger::tracing::warn!(
            capacity,
            evicted = excess,
            "rate limiter table full: forgot the least limited keys"
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, Instant>> {
        // The map stays consistent even if a holder panicked: every update is one insert.
        self.tats.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `Retry-After` seconds for a delay: whole seconds, rounded up, at least 1.
#[must_use]
pub fn retry_after_secs(delay: Duration) -> u64 {
    let secs = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
    secs.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    fn quota(burst: u32, period: Duration) -> Quota {
        Quota::new(NonZeroU32::new(burst).unwrap(), period).unwrap()
    }

    #[test]
    fn quota_rejects_a_zero_period() {
        assert_eq!(Quota::new(NonZeroU32::MIN, Duration::ZERO), None);
    }

    #[test]
    fn quota_per_window_spreads_the_refill() {
        let q = Quota::per(30, Duration::from_secs(60));
        assert_eq!(q.burst(), 30);
        assert_eq!(q.period(), 2 * SEC);
    }

    #[test]
    fn a_burst_is_allowed_then_the_next_request_is_limited() {
        let limiter = KeyedLimiter::new(quota(3, 10 * SEC), 10);
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(limiter.check(1, now), Ok(()));
        }
        assert_eq!(
            limiter.check(1, now),
            Err(Limited {
                retry_after: 10 * SEC
            })
        );
    }

    #[test]
    fn the_bucket_refills_one_request_per_period() {
        let limiter = KeyedLimiter::new(quota(2, 10 * SEC), 10);
        let start = Instant::now();
        limiter.check(1, start).unwrap();
        limiter.check(1, start).unwrap();
        let limited = limiter.check(1, start + 4 * SEC).unwrap_err();
        assert_eq!(limited.retry_after, 6 * SEC);
        // Exactly one request back after one period, not two.
        assert_eq!(limiter.check(1, start + 10 * SEC), Ok(()));
        assert!(limiter.check(1, start + 10 * SEC).is_err());
        // A long pause refills the whole burst, and no more.
        let later = start + 1000 * SEC;
        assert_eq!(limiter.check(1, later), Ok(()));
        assert_eq!(limiter.check(1, later), Ok(()));
        assert!(limiter.check(1, later).is_err());
    }

    #[test]
    fn refused_requests_do_not_push_the_limit_further() {
        let limiter = KeyedLimiter::new(quota(1, 10 * SEC), 10);
        let start = Instant::now();
        limiter.check(1, start).unwrap();
        for i in 1..10 {
            assert!(limiter.check(1, start + i * SEC).is_err());
        }
        assert_eq!(limiter.check(1, start + 10 * SEC), Ok(()));
    }

    #[test]
    fn keys_are_independent() {
        let limiter = KeyedLimiter::new(quota(1, 60 * SEC), 10);
        let now = Instant::now();
        assert_eq!(limiter.check("a", now), Ok(()));
        assert!(limiter.check("a", now).is_err());
        assert_eq!(limiter.check("b", now), Ok(()));
    }

    #[test]
    fn the_table_never_holds_more_than_its_capacity() {
        let limiter = KeyedLimiter::new(quota(5, 60 * SEC), 64);
        let now = Instant::now();
        for key in 0..10_000_u32 {
            assert_eq!(limiter.check(key, now), Ok(()), "a new key is allowed");
            assert!(limiter.len() <= 64);
        }
        assert!(limiter.len() > 64 - 64 / 8 - 1);
    }

    #[test]
    fn full_buckets_are_forgotten_first() {
        let limiter = KeyedLimiter::new(quota(1, 10 * SEC), 8);
        let start = Instant::now();
        for key in 0..8_u32 {
            limiter.check(key, start).unwrap();
        }
        // Key 0 is limited again just before the table fills up; the others refilled.
        let later = start + 10 * SEC;
        limiter.check(0, later).unwrap();
        limiter.check(100, later + SEC).unwrap();
        assert_eq!(limiter.len(), 2, "the refilled keys were dropped");
        assert!(
            limiter.check(0, later + SEC).is_err(),
            "key 0 is still limited"
        );
    }

    #[test]
    fn when_every_key_is_limited_the_least_limited_go_first() {
        let limiter = KeyedLimiter::new(quota(1, 100 * SEC), 8);
        let start = Instant::now();
        // Keys 0..8 are limited, key k until start + k s + 100 s.
        for key in 0..8_u32 {
            limiter.check(key, start + SEC * key).unwrap();
        }
        let now = start + 8 * SEC;
        limiter.check(100, now).unwrap();
        assert!(limiter.len() <= 8);
        // Key 0 (the closest to refilled) was forgotten; the most limited key was kept.
        assert_eq!(limiter.check(0, now), Ok(()));
        assert!(limiter.check(7, now).is_err());
    }

    #[test]
    fn a_capacity_of_zero_is_one() {
        let limiter = KeyedLimiter::new(quota(1, SEC), 0);
        assert_eq!(limiter.capacity(), 1);
        let now = Instant::now();
        limiter.check(1, now).unwrap();
        limiter.check(2, now).unwrap();
        assert_eq!(limiter.len(), 1);
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds_and_at_least_one() {
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(SEC), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1001)), 2);
        assert_eq!(retry_after_secs(59 * SEC), 59);
    }
}
