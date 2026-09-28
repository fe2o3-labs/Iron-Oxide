//! Slug identifiers of the program module: superset groups and built-in programs. Day ids are
//! [`DayId`](crate::DayId), next to [`ExerciseId`](crate::ExerciseId).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::slug_problem;

/// A slug identifier was rejected. Slugs follow the same rules as
/// [`ExerciseId`](crate::ExerciseId): 1 to 64 lowercase ASCII letters and digits, in words
/// separated by single hyphens.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
#[error("invalid {kind} `{value}`: {reason}")]
pub struct InvalidSlug {
    /// Which identifier was being parsed (e.g. `day id`).
    pub kind: &'static str,
    /// The rejected input.
    pub value: String,
    /// Why it was rejected.
    pub reason: &'static str,
}

macro_rules! slug_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates a slug.
            ///
            /// # Errors
            /// [`InvalidSlug`] when the text breaks one of the slug rules.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidSlug> {
                let value = value.into();
                match slug_problem(&value) {
                    None => Ok(Self(value)),
                    Some(reason) => Err(InvalidSlug {
                        kind: $kind,
                        value,
                        reason,
                    }),
                }
            }

            /// Returns the slug.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidSlug;

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
            type Err = InvalidSlug;

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
    /// Labels a superset inside a day (`a` for A1/A2). Exercises sharing a label form the group.
    SupersetId,
    "superset id"
);
slug_id!(
    /// Identifies a program shipped with the app (`full-body-3day`). Stable across releases, so a
    /// user's copy can record where it came from.
    BuiltinProgramId,
    "built-in program id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_slugs() {
        let group = SupersetId::new("upper-1").unwrap();
        assert_eq!(group.as_str(), "upper-1");
        assert_eq!(group.as_ref(), "upper-1");
        assert_eq!(group.to_string(), "upper-1");
        assert_eq!("upper-1".parse::<SupersetId>().unwrap(), group);
        assert_eq!(String::from(group.clone()), "upper-1");
        assert_eq!(SupersetId::try_from("upper-1".to_owned()).unwrap(), group);
        assert_eq!(
            BuiltinProgramId::new("full-body-3day").unwrap().as_str(),
            "full-body-3day"
        );
    }

    #[test]
    fn rejects_bad_slugs_with_the_kind() {
        assert_eq!(
            SupersetId::new("A 1").unwrap_err().to_string(),
            "invalid superset id `A 1`: only lowercase letters, digits and hyphens are allowed"
        );
        assert_eq!(
            SupersetId::new("").unwrap_err().to_string(),
            "invalid superset id ``: must not be empty"
        );
        assert_eq!(
            SupersetId::new("a--b").unwrap_err().to_string(),
            "invalid superset id `a--b`: must not contain consecutive hyphens"
        );
    }

    #[test]
    fn serde_is_a_validated_string() {
        let group = SupersetId::new("a").unwrap();
        assert_eq!(serde_json::to_string(&group).unwrap(), "\"a\"");
        assert_eq!(serde_json::from_str::<SupersetId>("\"a\"").unwrap(), group);
        let err = serde_json::from_str::<SupersetId>("\"-a\"").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("invalid superset id `-a`: must not start or end with a hyphen"),
            "{err}"
        );
    }
}
