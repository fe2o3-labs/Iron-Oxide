//! Program day identifiers.
//!
//! Provisional home: the program schema (#10) owns program days. Until it lands, [`DayId`] lives
//! here with the same slug rules as [`ExerciseId`](crate::ExerciseId), and moves to the program
//! module once both are merged.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::error::SessionError;

/// Identifies a day of a program with a stable slug such as `day-a` or `push`.
///
/// Rules: 1 to [`DayId::MAX_LEN`] characters, lowercase ASCII letters and digits, in words
/// separated by single hyphens (no leading, trailing or doubled hyphen).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DayId(String);

impl DayId {
    /// Maximum length of a slug, in bytes (all characters are ASCII).
    pub const MAX_LEN: usize = 64;

    /// Validates a slug.
    ///
    /// # Errors
    /// [`SessionError::InvalidDayId`] when the text breaks one of the slug rules.
    pub fn new(value: impl Into<String>) -> Result<Self, SessionError> {
        let value = value.into();
        match slug_problem(&value) {
            None => Ok(Self(value)),
            Some(reason) => Err(SessionError::InvalidDayId { value, reason }),
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
    if value.len() > DayId::MAX_LEN {
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

impl TryFrom<String> for DayId {
    type Error = SessionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DayId> for String {
    fn from(id: DayId) -> Self {
        id.0
    }
}

impl FromStr for DayId {
    type Err = SessionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for DayId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_slugs() {
        for slug in ["a", "day-a", "push-2", "5x5", &"d".repeat(DayId::MAX_LEN)] {
            let id = DayId::new(slug).unwrap();
            assert_eq!(id.as_str(), slug);
            assert_eq!(id.as_ref(), slug);
            assert_eq!(id.to_string(), slug);
            assert_eq!(slug.parse::<DayId>().unwrap(), id);
            assert_eq!(DayId::try_from(slug.to_owned()).unwrap(), id);
            assert_eq!(String::from(id), slug);
        }
    }

    #[test]
    fn rejects_invalid_slugs() {
        let too_long = "d".repeat(DayId::MAX_LEN + 1);
        let cases = [
            ("", "must not be empty"),
            (too_long.as_str(), "must be at most 64 characters"),
            (
                "Day-A",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            (
                "day a",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            (
                "jour-é",
                "only lowercase letters, digits and hyphens are allowed",
            ),
            ("-a", "must not start or end with a hyphen"),
            ("a-", "must not start or end with a hyphen"),
            ("day--a", "must not contain consecutive hyphens"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                DayId::new(input),
                Err(SessionError::InvalidDayId {
                    value: input.to_owned(),
                    reason: expected
                }),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn serde_is_a_validated_string() {
        let id = DayId::new("day-b").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"day-b\"");
        assert_eq!(serde_json::from_str::<DayId>("\"day-b\"").unwrap(), id);
        let err = serde_json::from_str::<DayId>("\"Day B\"").unwrap_err();
        assert!(err.to_string().contains("invalid day id `Day B`"));
    }
}
