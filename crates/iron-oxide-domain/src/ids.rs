//! Typed identifiers.
//!
//! Every entity gets its own UUID-backed newtype so that, for example, a [`SetId`] can never be passed
//! where a [`SessionId`] is expected. They serialize as a plain UUID string.
//!
//! [`ExerciseId`] and [`DayId`] are different: they are human-readable slugs (`back-squat`, `a`)
//! because exercises and days are named in hand-written program JSON and must stay stable across
//! program versions. Both follow the same slug rules.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ValueError;

#[cfg(feature = "uuid")]
mod v7;

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wraps an existing UUID (e.g. one read from the database).
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Generates a new time-ordered (version 7) ID.
            ///
            /// The first 48 bits are the current Unix time in milliseconds, so IDs sort by creation
            /// time and keep Postgres B-tree inserts at the end of the index. The creation time is
            /// therefore readable from the ID.
            ///
            /// Ordering guarantees:
            /// - Within one process, every ID (of any type) is strictly greater than all the IDs
            ///   generated before it, across threads, in the same millisecond, and when the clock
            ///   goes backwards. One process-wide lock covers the clock read and a 42-bit counter
            ///   that restarts from a random value each millisecond. When the clock is behind,
            ///   the last millisecond is reused and the counter incremented. The embedded time
            ///   then runs ahead of the clock by the whole step back (hours, if the clock was
            ///   set back by hours) until the clock catches up, so it is only an approximate
            ///   creation time.
            /// - Across processes (server and clients), IDs are only ordered to the millisecond of
            ///   their clocks.
            ///
            /// IDs are unique and ordered but **not unguessable**: the timestamp is readable, the
            /// counter is predictable after the first ID of a millisecond, and only the last 32
            /// bits are fresh randomness. Never use them as secrets, tokens or capability URLs.
            ///
            /// Works on every target: on `wasm32-unknown-unknown` the clock is `Date.now()` and the
            /// randomness `crypto.getRandomValues` (uuid's `js` feature); elsewhere it is
            /// `SystemTime` and the OS RNG.
            #[cfg(feature = "uuid")]
            #[must_use]
            pub fn new_v7() -> Self {
                Self(v7::next())
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s)
                    .map(Self)
                    .map_err(|source| ValueError::InvalidId { kind: $kind, source })
            }
        }
    };
}

uuid_id!(
    /// Identifies a user account.
    UserId,
    "user id"
);
uuid_id!(
    /// Identifies a training session.
    SessionId,
    "session id"
);
uuid_id!(
    /// Identifies a logged set. Generated on the client so that retried saves are idempotent.
    SetId,
    "set id"
);
uuid_id!(
    /// Identifies a program across all of its versions.
    ProgramId,
    "program id"
);
uuid_id!(
    /// Identifies one immutable version of a program.
    ProgramVersionId,
    "program version id"
);

/// Maximum length of a slug ID ([`ExerciseId`], [`DayId`]), in bytes (all characters are ASCII).
pub const SLUG_MAX_LEN: usize = 64;

/// Returns why `value` is not a valid slug, or `None` when it is. Shared by every slug ID.
pub(crate) fn slug_problem(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("must not be empty");
    }
    if value.len() > SLUG_MAX_LEN {
        return Some("must be at most 64 characters");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Some("only lowercase letters, digits and hyphens are allowed");
    }
    if value.starts_with('-') || value.ends_with('-') {
        return Some("must not start or end with a hyphen");
    }
    if value.contains("--") {
        return Some("must not contain consecutive hyphens");
    }
    None
}

macro_rules! slug_id {
    ($(#[$doc:meta])* $name:ident, $variant:ident) => {
        $(#[$doc])*
        ///
        /// Rules: 1 to [`SLUG_MAX_LEN`] characters, lowercase ASCII letters and digits, in words
        /// separated by single hyphens (no leading, trailing or doubled hyphen). Serializes as a plain
        /// string and is validated on deserialize.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Maximum length of the slug, in bytes (all characters are ASCII).
            pub const MAX_LEN: usize = SLUG_MAX_LEN;

            /// Validates a slug.
            ///
            /// # Errors
            #[doc = concat!("[`ValueError::", stringify!($variant), "`] when the text breaks one of the slug rules.")]
            pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
                let value = value.into();
                match slug_problem(&value) {
                    None => Ok(Self(value)),
                    Some(reason) => Err(ValueError::$variant { value, reason }),
                }
            }

            /// Returns the slug.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValueError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

slug_id!(
    /// Identifies an exercise with a stable slug such as `back-squat` or `ohp`.
    ExerciseId,
    InvalidExerciseId
);
slug_id!(
    /// Identifies a training day of a program with a stable slug such as `a` or `upper-1`.
    DayId,
    InvalidDayId
);

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "67e55044-10b1-426f-9247-bb680e5fe0c8";

    #[test]
    fn uuid_ids_round_trip_through_uuid_display_and_from_str() {
        let uuid = Uuid::parse_str(RAW).unwrap();
        let id = SessionId::from_uuid(uuid);
        assert_eq!(id.as_uuid(), uuid);
        assert_eq!(Uuid::from(id), uuid);
        assert_eq!(SessionId::from(uuid), id);
        assert_eq!(id.to_string(), RAW);
        assert_eq!(RAW.parse::<SessionId>().unwrap(), id);
    }

    #[test]
    fn uuid_ids_serialize_as_a_bare_string() {
        let id = SetId::from_uuid(Uuid::parse_str(RAW).unwrap());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{RAW}\""));
        assert_eq!(serde_json::from_str::<SetId>(&json).unwrap(), id);
    }

    #[test]
    fn uuid_ids_reject_invalid_text() {
        let err = "not-a-uuid".parse::<ProgramVersionId>().unwrap_err();
        assert!(matches!(
            err,
            ValueError::InvalidId {
                kind: "program version id",
                ..
            }
        ));
        assert!(err.to_string().starts_with("invalid program version id: "));
        assert!(serde_json::from_str::<UserId>("\"nope\"").is_err());
        assert!(serde_json::from_str::<ProgramId>("42").is_err());
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn new_v7_generates_rfc_version_7_ids() {
        for uuid in [
            UserId::new_v7().as_uuid(),
            SessionId::new_v7().as_uuid(),
            SetId::new_v7().as_uuid(),
            ProgramId::new_v7().as_uuid(),
            ProgramVersionId::new_v7().as_uuid(),
        ] {
            assert_eq!(uuid.get_version_num(), 7);
            assert_eq!(uuid.get_version(), Some(uuid::Version::SortRand));
            assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        }
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn new_v7_embeds_the_current_unix_time_in_milliseconds() {
        fn unix_ms() -> u128 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        }
        let before = unix_ms();
        let id = SessionId::new_v7();
        let after = unix_ms();
        let (secs, nanos) = id.as_uuid().get_timestamp().unwrap().to_unix();
        let embedded = u128::from(secs) * 1_000 + u128::from(nanos) / 1_000_000;
        // uuid may bump the timestamp by a millisecond if its counter overflows; allow a margin.
        assert!(
            before <= embedded && embedded <= after + 5,
            "{before} <= {embedded} <= {after}"
        );
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn new_v7_is_strictly_increasing_across_a_burst() {
        // 10k IDs are generated in a few milliseconds, so many share a millisecond: the ordering
        // within one comes from the counter. Tests running in parallel share the process-wide
        // generator, which only removes values from this sequence.
        let ids: Vec<SetId> = (0..10_000).map(|_| SetId::new_v7()).collect();
        for pair in ids.windows(2) {
            assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
            assert!(pair[0].to_string() < pair[1].to_string());
        }
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn new_v7_is_strictly_increasing_per_thread_and_unique_across_threads() {
        // Runs long enough to cross at least one second boundary, where uuid's own `now_v7`
        // lets a thread's ids go backwards.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1_100);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    let mut ids = Vec::new();
                    while ids.len() < 20_000 || std::time::Instant::now() < deadline {
                        ids.push(SetId::new_v7());
                    }
                    ids
                })
            })
            .collect();
        let mut all = Vec::new();
        for thread in threads {
            let ids = thread.join().unwrap();
            for pair in ids.windows(2) {
                assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
            }
            all.extend(ids);
        }
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "duplicate ids");
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn new_v7_ids_serialize_as_a_bare_string() {
        let id = SetId::new_v7();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_uuid().hyphenated()));
        assert_eq!(serde_json::from_str::<SetId>(&json).unwrap(), id);
    }

    #[test]
    fn exercise_id_accepts_valid_slugs() {
        for slug in [
            "ohp",
            "back-squat",
            "a",
            "3x-plank-2",
            &"a".repeat(ExerciseId::MAX_LEN),
        ] {
            let id = ExerciseId::new(slug).unwrap();
            assert_eq!(id.as_str(), slug);
            assert_eq!(id.as_ref(), slug);
            assert_eq!(id.to_string(), slug);
            assert_eq!(slug.parse::<ExerciseId>().unwrap(), id);
        }
    }

    #[test]
    fn exercise_id_rejects_invalid_slugs() {
        let cases = [
            ("", "must not be empty"),
            (
                &"a".repeat(ExerciseId::MAX_LEN + 1),
                "must be at most 64 characters",
            ),
            (
                "Back-Squat",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            (
                "back squat",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            (
                "bänk",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            (
                "back_squat",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            ("-squat", "must not start or end with a hyphen"),
            ("squat-", "must not start or end with a hyphen"),
            ("back--squat", "must not contain consecutive hyphens"),
        ];
        for (input, expected) in cases {
            match ExerciseId::new(input) {
                Err(ValueError::InvalidExerciseId { value, reason }) => {
                    assert_eq!(value, input);
                    assert_eq!(reason, expected, "input {input:?}");
                }
                other => panic!("{input:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn day_id_follows_the_same_rules() {
        assert_eq!(DayId::MAX_LEN, ExerciseId::MAX_LEN);
        for slug in ["a", "upper-1", "day-b", &"d".repeat(DayId::MAX_LEN)] {
            let id = DayId::new(slug).unwrap();
            assert_eq!(id.as_str(), slug);
            assert_eq!(id.as_ref(), slug);
            assert_eq!(id.to_string(), slug);
            assert_eq!(slug.parse::<DayId>().unwrap(), id);
            assert_eq!(DayId::try_from(slug.to_owned()).unwrap(), id);
        }
        let cases = [
            ("", "must not be empty"),
            (
                &"d".repeat(DayId::MAX_LEN + 1),
                "must be at most 64 characters",
            ),
            (
                "Day A",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            ("-a", "must not start or end with a hyphen"),
            ("a-", "must not start or end with a hyphen"),
            ("day--a", "must not contain consecutive hyphens"),
        ];
        for (input, expected) in cases {
            match DayId::new(input) {
                Err(ValueError::InvalidDayId { value, reason }) => {
                    assert_eq!(value, input);
                    assert_eq!(reason, expected, "input {input:?}");
                }
                other => panic!("{input:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn day_id_serde_validates() {
        let id = DayId::new("upper-1").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"upper-1\"");
        assert_eq!(serde_json::from_str::<DayId>("\"upper-1\"").unwrap(), id);
        assert_eq!(String::from(id), "upper-1");
        let err = serde_json::from_str::<DayId>("\"Day A\"").unwrap_err();
        assert!(err.to_string().contains("invalid day id `Day A`"));
        assert!(serde_json::from_str::<DayId>("1").is_err());
    }

    #[test]
    fn exercise_id_serde_validates() {
        let id = ExerciseId::new("deadlift").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"deadlift\"");
        assert_eq!(
            serde_json::from_str::<ExerciseId>("\"deadlift\"").unwrap(),
            id
        );
        assert_eq!(String::from(id), "deadlift");
        let err = serde_json::from_str::<ExerciseId>("\"Dead Lift\"").unwrap_err();
        assert!(err.to_string().contains("invalid exercise id `Dead Lift`"));
    }
}
