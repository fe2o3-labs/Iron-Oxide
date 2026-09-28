//! Process-wide generator of strictly increasing version 7 UUIDs.
//!
//! uuid's own `Uuid::now_v7()` does not guarantee order within a process. It reads the clock
//! before taking its context lock, so a thread holding a stale reading from just before a second
//! boundary can reach the context after another thread has moved it into the next second. Its
//! `ContextV7` also compares the sub-second part across seconds, so that stale reading produces an
//! id about 1 s in the future, and the next id sorts before it. This generator reads the clock
//! *inside* its lock and never emits an `(ms, counter)` pair that is not greater than the last one.
//!
//! Layout of an id: 48-bit Unix milliseconds, the version, a 42-bit counter (12 bits in `rand_a`
//! and the top 30 bits of `rand_b`, placed by [`Uuid::new_v7`]), then 32 random bits. Ids sort
//! by `(ms, counter)`.

use std::sync::{Mutex, PoisonError};

use uuid::{NoContext, Timestamp, Uuid};

/// Width of the counter embedded in every id.
const COUNTER_BITS: u8 = 42;
/// Largest counter value that still fits in [`COUNTER_BITS`].
const MAX_COUNTER: u64 = (1 << COUNTER_BITS) - 1;
/// A fresh counter uses the lower 41 bits only, so at least 2^41 ids fit in one millisecond
/// before the generator has to borrow the next one.
const SEED_MASK: u64 = MAX_COUNTER >> 1;

/// State of the process-wide generator: the last `(ms, counter)` it emitted.
#[derive(Debug)]
pub(super) struct Generator {
    last_ms: u64,
    counter: u64,
}

impl Generator {
    pub(super) const fn new() -> Self {
        Self {
            last_ms: 0,
            counter: 0,
        }
    }

    /// Returns the `(ms, counter)` of the next id, strictly greater than the previous one.
    ///
    /// - `now_ms` after the last millisecond: use it and start from a fresh random counter.
    /// - `now_ms` equal to or behind it (a burst, or a clock that went backwards): keep the last
    ///   millisecond and increment the counter.
    /// - Counter exhausted: move to the next millisecond with a fresh counter.
    ///
    /// The new pair is computed first and stored in one assignment, so if `seed` panics the state
    /// is left exactly as it was.
    pub(super) fn advance(&mut self, now_ms: u64, seed: impl FnOnce() -> u64) -> (u64, u64) {
        let (last_ms, counter) = if now_ms > self.last_ms {
            (now_ms, seed() & SEED_MASK)
        } else if self.counter < MAX_COUNTER {
            (self.last_ms, self.counter + 1)
        } else {
            (self.last_ms.saturating_add(1), seed() & SEED_MASK)
        };
        *self = Self { last_ms, counter };
        (last_ms, counter)
    }
}

/// Builds the id for a `(ms, counter)` pair; the bits after the counter are random.
pub(super) fn build(ms: u64, counter: u64) -> Uuid {
    // Sub-millisecond precision is dropped: only the milliseconds end up in the id.
    let subsec_nanos = u32::try_from((ms % 1_000) * 1_000_000).unwrap_or(0);
    Uuid::new_v7(Timestamp::from_unix_time(
        ms / 1_000,
        subsec_nanos,
        u128::from(counter),
        COUNTER_BITS,
    ))
}

/// Current Unix time in milliseconds. Uses uuid's clock: `Date.now()` on `wasm32-unknown-unknown`
/// (the `js` feature), `SystemTime` elsewhere. `NoContext` leaves the reading untouched.
fn now_ms() -> u64 {
    let (seconds, subsec_nanos) = Timestamp::now(NoContext).to_unix();
    seconds
        .saturating_mul(1_000)
        .saturating_add(u64::from(subsec_nanos / 1_000_000))
}

/// 64 random bits from uuid's RNG (`crypto.getRandomValues` on wasm, the OS elsewhere): the last
/// 62 bits of a v7 id without a counter are random, which is all [`SEED_MASK`] keeps.
fn random_seed() -> u64 {
    Uuid::new_v7(Timestamp::from_unix_time(0, 0, 0, 0))
        .as_u64_pair()
        .1
}

static GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());

/// Generates the next id. The clock is read while the lock is held, so the order of the ids is
/// the order in which callers took the lock.
pub(super) fn next() -> Uuid {
    // A poisoned lock is still usable: `advance` either stores a complete new `(ms, counter)` or,
    // if the RNG panics, leaves the previous one untouched, so the state is always one that was
    // actually emitted.
    let mut generator = GENERATOR.lock().unwrap_or_else(PoisonError::into_inner);
    let (ms, counter) = generator.advance(now_ms(), random_seed);
    drop(generator);
    build(ms, counter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms_of(uuid: Uuid) -> u64 {
        let (seconds, nanos) = uuid.get_timestamp().unwrap().to_unix();
        seconds * 1_000 + u64::from(nanos / 1_000_000)
    }

    /// Feeds `clock` through a fresh generator with a fixed seed and returns the ids.
    fn ids_for(clock: &[u64], seed: u64) -> Vec<Uuid> {
        let mut generator = Generator::new();
        clock
            .iter()
            .map(|&now| {
                let (ms, counter) = generator.advance(now, || seed);
                build(ms, counter)
            })
            .collect()
    }

    fn assert_strictly_increasing(ids: &[Uuid]) {
        for pair in ids.windows(2) {
            assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn a_new_millisecond_uses_the_clock_and_a_masked_seed() {
        let mut generator = Generator::new();
        assert_eq!(generator.advance(5, || u64::MAX), (5, SEED_MASK));
        assert_eq!(generator.advance(9, || 3), (9, 3));
    }

    #[test]
    fn the_same_millisecond_increments_the_counter() {
        let mut generator = Generator::new();
        assert_eq!(generator.advance(5, || 10), (5, 10));
        assert_eq!(generator.advance(5, || unreachable!()), (5, 11));
        assert_eq!(generator.advance(5, || unreachable!()), (5, 12));
    }

    #[test]
    fn a_panicking_seed_leaves_the_state_unchanged() {
        let mut generator = Generator::new();
        assert_eq!(generator.advance(5, || 10), (5, 10));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            generator.advance(9, || panic!("rng failure"))
        }));
        assert!(result.is_err());
        assert_eq!((generator.last_ms, generator.counter), (5, 10));
        assert_eq!(generator.advance(5, || unreachable!()), (5, 11));
    }

    #[test]
    fn a_clock_going_backwards_keeps_the_last_millisecond() {
        let mut generator = Generator::new();
        assert_eq!(generator.advance(1_000, || 10), (1_000, 10));
        assert_eq!(generator.advance(400, || unreachable!()), (1_000, 11));
        assert_eq!(generator.advance(1_001, || 2), (1_001, 2));
    }

    #[test]
    fn an_exhausted_counter_borrows_the_next_millisecond() {
        let mut generator = Generator {
            last_ms: 7,
            counter: MAX_COUNTER - 1,
        };
        assert_eq!(generator.advance(7, || unreachable!()), (7, MAX_COUNTER));
        assert_eq!(generator.advance(7, || 4), (8, 4));
        // The clock catching up with the borrowed millisecond does not go back to it.
        assert_eq!(generator.advance(8, || unreachable!()), (8, 5));
        assert_eq!(generator.advance(9, || 1), (9, 1));
    }

    #[test]
    fn ids_stay_ordered_across_a_second_boundary_and_a_backwards_clock() {
        // The sequence that breaks uuid's `ContextV7`: 1000.100 s, a stale 999.900 s, 1000.500 s.
        // Then a step back across the next second and a recovery.
        let clock = [
            1_000_100, 999_900, 1_000_500, 1_001_000, 999_000, 1_001_000, 1_001_001,
        ];
        let ids = ids_for(&clock, 0);
        assert_strictly_increasing(&ids);
        let embedded: Vec<u64> = ids.iter().map(|&id| ms_of(id)).collect();
        assert_eq!(
            embedded,
            [
                1_000_100, 1_000_100, 1_000_500, 1_001_000, 1_001_000, 1_001_000, 1_001_001
            ]
        );
    }

    #[test]
    fn built_ids_are_rfc_version_7_with_the_counter_in_order() {
        let mut ids = vec![build(1, 0), build(1, 1), build(1, MAX_COUNTER), build(2, 0)];
        ids.push(build(1_790_000_000_000, SEED_MASK));
        for &id in &ids {
            assert_eq!(id.get_version_num(), 7);
            assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
        }
        assert_strictly_increasing(&ids);
        assert_eq!(ms_of(ids[4]), 1_790_000_000_000);
    }

    #[test]
    fn the_counter_is_embedded_in_the_top_42_bits_after_the_timestamp() {
        let id = build(1, MAX_COUNTER).as_u128();
        // rand_a (bits 64..76) and the first 30 bits of rand_b (bits 32..62) are all ones.
        assert_eq!((id >> 64) & 0xfff, 0xfff);
        assert_eq!((id >> 32) & ((1 << 30) - 1), (1 << 30) - 1);
        let zero = build(1, 0).as_u128();
        assert_eq!((zero >> 64) & 0xfff, 0);
        assert_eq!((zero >> 32) & ((1 << 30) - 1), 0);
    }

    #[test]
    fn the_real_clock_is_close_to_system_time() {
        let before = now_ms();
        let system = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let after = now_ms();
        assert!(
            before <= system && system <= after,
            "{before} {system} {after}"
        );
    }

    #[test]
    fn random_seeds_differ() {
        assert_ne!(random_seed() & SEED_MASK, random_seed() & SEED_MASK);
    }
}
