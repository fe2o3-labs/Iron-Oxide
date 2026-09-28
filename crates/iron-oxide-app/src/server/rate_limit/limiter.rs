//! A keyed token bucket (GCRA) with a hard cap on the number of keys it remembers.
//!
//! Each key holds one instant, its "theoretical arrival time" (TAT): the moment its bucket will
//! be full again. A request at `now` is allowed when `max(TAT, now) + period` is at most
//! `now + burst × period`, and then moves the TAT to that value. A refused request changes
//! nothing, so hammering a limit does not push it further away.
//!
//! A key whose TAT is in the past has a full bucket, exactly like a key never seen: forgetting it
//! loses nothing, and those are the only keys ever forgotten. Memory is bounded by count, not by
//! time: the table holds at most [`KeyedLimiter::capacity`] keys. When a new key arrives and the
//! table is full, the full-bucket keys are dropped. If none are, no key is evicted (evicting a key
//! that is being limited would hand it a fresh burst, so cycling through more keys than the table
//! holds would have no limit at all) and the new key gets the limiter's [`WhenFull`] policy.
//!
//! The sweep scans the table, so it only runs when it can free something: while the table is
//! full, the limiter remembers the earliest moment any stored key refills and does not sweep
//! before it. A flood of new keys therefore cannot turn the sweep into a CPU sink.

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

/// What happens to a new key when the table is full and no stored key has refilled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenFull {
    /// Refuse it (fail closed), until the earliest stored key refills.
    Refuse,
    /// Let the request through without tracking the key (fail open), and log it.
    Allow,
}

/// A per-key rate limit. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct KeyedLimiter<K> {
    quota: Quota,
    capacity: usize,
    when_full: WhenFull,
    table: Mutex<Table<K>>,
}

#[derive(Debug)]
struct Table<K> {
    tats: HashMap<K, Instant>,
    /// Set while the table is full of keys that have not refilled: the earliest TAT among them.
    /// Stored TATs only grow and nothing is inserted while full, so no key refills before it.
    full_until: Option<Instant>,
}

impl<K: Hash + Eq + Copy> KeyedLimiter<K> {
    /// A limiter remembering at most `capacity` keys (at least 1).
    #[must_use]
    pub fn new(quota: Quota, capacity: usize, when_full: WhenFull) -> Self {
        Self {
            quota,
            capacity: capacity.max(1),
            when_full,
            table: Mutex::new(Table {
                tats: HashMap::new(),
                full_until: None,
            }),
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
        self.lock().tats.len()
    }

    /// Counts one request from `key` at `now`.
    pub fn check(&self, key: K, now: Instant) -> Result<(), Limited> {
        let quota = self.quota;
        let mut table = self.lock();
        let stored = table.tats.get(&key).copied();
        let tat = stored.unwrap_or(now).max(now);
        let next = tat + quota.period;
        let ahead = next.saturating_duration_since(now);
        if ahead > quota.tolerance() {
            return Err(Limited {
                retry_after: ahead - quota.tolerance(),
            });
        }
        if stored.is_none()
            && table.tats.len() >= self.capacity
            && let Some(full_until) = self.make_room(&mut table, now)
        {
            return match self.when_full {
                WhenFull::Refuse => Err(Limited {
                    retry_after: full_until.saturating_duration_since(now),
                }),
                WhenFull::Allow => Ok(()),
            };
        }
        table.tats.insert(key, next);
        Ok(())
    }

    /// Drops the keys whose bucket has refilled. Returns `None` if there is room now, else the
    /// earliest moment a stored key refills.
    fn make_room(&self, table: &mut Table<K>, now: Instant) -> Option<Instant> {
        if let Some(full_until) = table.full_until
            && now < full_until
        {
            return Some(full_until);
        }
        // Full buckets carry no state.
        table.tats.retain(|_, tat| *tat > now);
        if table.tats.len() < self.capacity {
            table.full_until = None;
            return None;
        }
        let earliest = table.tats.values().min().copied().unwrap_or(now);
        table.full_until = Some(earliest);
        dioxus::logger::tracing::warn!(
            capacity = self.capacity,
            policy = ?self.when_full,
            "rate limiter table full of limited keys: new keys are refused or let through untracked"
        );
        Some(earliest)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table<K>> {
        // The table stays consistent even if a holder panicked: every update is one statement.
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
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
        let limiter = KeyedLimiter::new(quota(3, 10 * SEC), 10, WhenFull::Refuse);
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
        let limiter = KeyedLimiter::new(quota(2, 10 * SEC), 10, WhenFull::Refuse);
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
        let limiter = KeyedLimiter::new(quota(1, 10 * SEC), 10, WhenFull::Refuse);
        let start = Instant::now();
        limiter.check(1, start).unwrap();
        for i in 1..10 {
            assert!(limiter.check(1, start + i * SEC).is_err());
        }
        assert_eq!(limiter.check(1, start + 10 * SEC), Ok(()));
    }

    #[test]
    fn keys_are_independent() {
        let limiter = KeyedLimiter::new(quota(1, 60 * SEC), 10, WhenFull::Refuse);
        let now = Instant::now();
        assert_eq!(limiter.check("a", now), Ok(()));
        assert!(limiter.check("a", now).is_err());
        assert_eq!(limiter.check("b", now), Ok(()));
    }

    #[test]
    fn the_table_never_holds_more_than_its_capacity() {
        for when_full in [WhenFull::Refuse, WhenFull::Allow] {
            let limiter = KeyedLimiter::new(quota(5, 60 * SEC), 64, when_full);
            let now = Instant::now();
            for key in 0..10_000_u32 {
                let _ = limiter.check(key, now);
                assert!(limiter.len() <= 64);
            }
            assert_eq!(limiter.len(), 64);
        }
    }

    #[test]
    fn full_buckets_are_forgotten_to_make_room() {
        let limiter = KeyedLimiter::new(quota(1, 10 * SEC), 8, WhenFull::Refuse);
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

    /// The reviewer's key-cycling attack: more drained keys than the table holds, clock frozen.
    /// Evicting limited keys would hand them fresh bursts round after round.
    #[test]
    fn cycling_through_more_keys_than_the_table_holds_gains_nothing() {
        for when_full in [WhenFull::Refuse, WhenFull::Allow] {
            let limiter = KeyedLimiter::new(quota(5, 12 * 60 * SEC), 64, when_full);
            let now = Instant::now();
            let mut allowed_per_key = [0_u32; 80];
            for _round in 0..100 {
                for key in 0..80_u32 {
                    for _ in 0..5 {
                        if limiter.check(key, now).is_ok() {
                            allowed_per_key[key as usize] += 1;
                        }
                    }
                }
            }
            // The 64 keys that got in used their burst once, and never got another.
            assert!(
                allowed_per_key[..64].iter().all(|n| *n == 5),
                "{when_full:?}"
            );
            let late: u32 = allowed_per_key[64..].iter().sum();
            match when_full {
                // Fail closed: the keys that found the table full got nothing.
                WhenFull::Refuse => assert_eq!(late, 0),
                // Fail open: they went through untracked, and no tracked key was reset.
                WhenFull::Allow => assert_eq!(late, 16 * 5 * 100),
            }
        }
    }

    #[test]
    fn a_full_table_refuses_new_keys_until_a_stored_key_refills() {
        let limiter = KeyedLimiter::new(quota(1, 100 * SEC), 8, WhenFull::Refuse);
        let start = Instant::now();
        // Keys 0..8 are limited, key k until start + k s + 100 s.
        for key in 0..8_u32 {
            limiter.check(key, start + SEC * key).unwrap();
        }
        let now = start + 8 * SEC;
        let refused = limiter.check(100, now).unwrap_err();
        assert_eq!(refused.retry_after, 92 * SEC, "until key 0 refills");
        // Every stored key is still limited: none was evicted.
        for key in 0..8_u32 {
            assert!(limiter.check(key, now).is_err(), "key {key}");
        }
        assert!(limiter.check(100, start + 99 * SEC).is_err());
        // Key 0 refills at start + 100 s: its slot goes to the new key.
        assert_eq!(limiter.check(100, start + 100 * SEC), Ok(()));
        assert_eq!(limiter.len(), 8);
        assert!(limiter.check(1, start + 100 * SEC).is_err());
    }

    #[test]
    fn a_full_table_lets_new_keys_through_untracked_when_failing_open() {
        let limiter = KeyedLimiter::new(quota(1, 100 * SEC), 4, WhenFull::Allow);
        let now = Instant::now();
        for key in 0..4_u32 {
            limiter.check(key, now).unwrap();
        }
        for _ in 0..10 {
            assert_eq!(limiter.check(100, now), Ok(()));
        }
        assert_eq!(limiter.len(), 4);
        for key in 0..4_u32 {
            assert!(limiter.check(key, now).is_err(), "key {key} stays limited");
        }
    }

    #[test]
    fn a_capacity_of_zero_is_one() {
        let limiter = KeyedLimiter::new(quota(1, SEC), 0, WhenFull::Refuse);
        assert_eq!(limiter.capacity(), 1);
        let now = Instant::now();
        limiter.check(1, now).unwrap();
        assert!(limiter.check(2, now).is_err());
        assert_eq!(limiter.check(2, now + SEC), Ok(()));
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
