//! Errors raised by the session model and the day rotation.

use crate::ids::{SessionId, SetId};

use super::day::DayId;
use super::model::SessionStatus;

/// A session, a logged set or a day id broke one of the session rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// A day ID is not a valid slug.
    #[error("invalid day id `{value}`: {reason}")]
    InvalidDayId {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The session has already ended, so it cannot take a new set or end a second time differently.
    #[error("session {session_id} has already ended ({status})")]
    AlreadyEnded {
        /// The session.
        session_id: SessionId,
        /// How it ended.
        status: SessionStatus,
    },
    /// A set with this ID was already logged with different values.
    #[error("set {set_id} was already logged with different values")]
    SetConflict {
        /// The client-generated set ID.
        set_id: SetId,
    },
    /// The same set ID appears twice in a stored session.
    #[error("set {set_id} appears more than once")]
    DuplicateSetId {
        /// The repeated set ID.
        set_id: SetId,
    },
    /// A set is timestamped before its session started.
    #[error("set {set_id} was completed before the session started")]
    SetBeforeStart {
        /// The offending set.
        set_id: SetId,
    },
    /// The end time is before the session start.
    #[error("session {session_id} cannot end before it started")]
    EndBeforeStart {
        /// The session.
        session_id: SessionId,
    },
    /// The end time is before a logged set was completed.
    #[error("session {session_id} cannot end before set {set_id} was completed")]
    EndBeforeSet {
        /// The session.
        session_id: SessionId,
        /// The set completed after the requested end time.
        set_id: SetId,
    },
    /// A stored status and end time disagree: an in-progress session with an end time, or an ended
    /// session without one.
    #[error("session {session_id} is {status} but its end time is {}", if *.has_end { "set" } else { "missing" })]
    InconsistentEnd {
        /// The session.
        session_id: SessionId,
        /// The stored status.
        status: SessionStatus,
        /// Whether an end time was stored.
        has_end: bool,
    },
}

/// The next day of a rotation cannot be chosen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RotationError {
    /// The program has no days.
    #[error("the rotation has no days")]
    EmptyRotation,
    /// A day appears more than once, so "the day after it" is ambiguous.
    #[error("day `{0}` appears more than once in the rotation")]
    DuplicateDay(DayId),
}
