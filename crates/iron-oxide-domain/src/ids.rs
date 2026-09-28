//! Typed identifiers.
//!
//! Every entity gets its own UUID-backed newtype so that, for example, a [`SetId`] can never be passed
//! where a [`SessionId`] is expected. They serialize as a plain UUID string.
//!
//! [`ExerciseId`] is different: it is a human-readable slug (`back-squat`) because exercises are named
//! in hand-written program JSON and must stay stable across program versions.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ValueError;

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

            /// Generates a new random (version 4) ID.
            ///
            /// On `wasm32-unknown-unknown` the randomness comes from `crypto.getRandomValues`.
            #[cfg(feature = "uuid")]
            #[must_use]
            pub fn new_v4() -> Self {
                Self(Uuid::new_v4())
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

/// Identifies an exercise with a stable slug such as `back-squat` or `ohp`.
///
/// Rules: 1 to [`ExerciseId::MAX_LEN`] characters, lowercase ASCII letters and digits, in words
/// separated by single hyphens (no leading, trailing or doubled hyphen).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExerciseId(String);

impl ExerciseId {
    /// Maximum length of a slug, in bytes (all characters are ASCII).
    pub const MAX_LEN: usize = 64;

    /// Validates a slug.
    ///
    /// # Errors
    /// [`ValueError::InvalidExerciseId`] when the text breaks one of the slug rules.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        match slug_problem(&value) {
            None => Ok(Self(value)),
            Some(reason) => Err(ValueError::InvalidExerciseId { value, reason }),
        }
    }

    /// Returns the slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn slug_problem(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("must not be empty");
    }
    if value.len() > ExerciseId::MAX_LEN {
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

impl TryFrom<String> for ExerciseId {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ExerciseId> for String {
    fn from(id: ExerciseId) -> Self {
        id.0
    }
}

impl FromStr for ExerciseId {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for ExerciseId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExerciseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
    fn new_v4_generates_distinct_version_4_ids() {
        let a = UserId::new_v4();
        let b = UserId::new_v4();
        assert_ne!(a, b);
        assert_eq!(a.as_uuid().get_version_num(), 4);
        assert_eq!(ProgramId::new_v4().as_uuid().get_version_num(), 4);
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
