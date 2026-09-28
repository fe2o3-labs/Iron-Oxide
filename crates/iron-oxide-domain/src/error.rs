//! Errors raised when a raw value cannot become a domain value.

use std::fmt;

/// The kind of quantity an error is about, used to build readable messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantity {
    /// A [`Weight`](crate::Weight).
    Weight,
    /// A [`Percent`](crate::Percent).
    Percent,
    /// A [`Reps`](crate::Reps) count.
    Reps,
    /// A [`Seconds`](crate::Seconds) duration.
    Seconds,
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Weight => "weight",
            Self::Percent => "percent",
            Self::Reps => "reps",
            Self::Seconds => "duration",
        };
        f.write_str(name)
    }
}

/// A raw value was rejected by a validating constructor or an arithmetic operation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValueError {
    /// The input was NaN or infinite.
    #[error("{quantity} must be a finite number")]
    NotFinite {
        /// What was being built.
        quantity: Quantity,
    },
    /// The input was below zero.
    #[error("{quantity} must not be negative (got {value})")]
    Negative {
        /// What was being built.
        quantity: Quantity,
        /// The rejected input.
        value: f64,
    },
    /// The input or the result of an operation exceeds the allowed maximum.
    #[error("{quantity} must be at most {max}")]
    TooLarge {
        /// What was being built.
        quantity: Quantity,
        /// The maximum, formatted with its unit (e.g. `2000 kg`).
        max: &'static str,
    },
    /// A rounding increment of zero was requested.
    #[error("rounding increment must be greater than zero")]
    ZeroIncrement,
    /// The unit text is neither `kg` nor `lb`.
    #[error("unknown unit `{0}` (expected `kg` or `lb`)")]
    UnknownUnit(String),
    /// A textual ID is not a valid UUID.
    #[error("invalid {kind}: {source}")]
    InvalidId {
        /// Which ID type was being parsed (e.g. `session id`).
        kind: &'static str,
        /// The underlying parse error.
        #[source]
        source: uuid::Error,
    },
    /// An exercise ID is not a valid slug.
    #[error("invalid exercise id `{value}`: {reason}")]
    InvalidExerciseId {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A program day ID is not a valid slug.
    #[error("invalid day id `{value}`: {reason}")]
    InvalidDayId {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },
}
