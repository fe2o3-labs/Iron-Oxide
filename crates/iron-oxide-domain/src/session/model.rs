//! A training session header and its lifecycle status.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{DayId, ProgramVersionId, SessionId};

use super::error::SessionError;

/// Where a session is in its lifecycle.
///
/// Serializes as `in_progress`, `completed`, `skipped` or `abandoned`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Started and not ended yet: sets can still be logged.
    InProgress,
    /// Trained and finished normally. Advances the day rotation.
    Completed,
    /// The user deliberately passed over this day. Advances the day rotation.
    Skipped,
    /// Given up part-way (or discarded). Does **not** advance the rotation: the day comes up again.
    Abandoned,
}

impl SessionStatus {
    /// Whether the session has ended (any status but [`SessionStatus::InProgress`]).
    #[must_use]
    pub const fn is_ended(self) -> bool {
        !matches!(self, Self::InProgress)
    }

    /// Whether a session with this status moves the rotation on to the next day.
    #[must_use]
    pub const fn advances_rotation(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped)
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::InProgress => "in progress",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Abandoned => "abandoned",
        };
        f.write_str(text)
    }
}

/// How a session ends: every [`SessionStatus`] except `InProgress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    /// See [`SessionStatus::Completed`].
    Completed,
    /// See [`SessionStatus::Skipped`].
    Skipped,
    /// See [`SessionStatus::Abandoned`].
    Abandoned,
}

impl From<SessionOutcome> for SessionStatus {
    fn from(outcome: SessionOutcome) -> Self {
        match outcome {
            SessionOutcome::Completed => Self::Completed,
            SessionOutcome::Skipped => Self::Skipped,
            SessionOutcome::Abandoned => Self::Abandoned,
        }
    }
}

/// Whether an operation changed anything. Retried operations report [`Change::Unchanged`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// The operation was applied.
    Applied,
    /// The exact same operation had already been applied: nothing changed.
    Unchanged,
}

/// A training session of one program day, linked to the immutable program version it was run from.
///
/// `T` is the timestamp type: any totally ordered point in time (the crate's UTC epoch-millisecond
/// timestamp in the app, a plain integer in tests).
///
/// Invariants, enforced by every constructor and by deserialization:
/// - `finished_at` is set exactly when the status is not [`SessionStatus::InProgress`];
/// - `finished_at` is not before `started_at`.
///
/// A session only ends through [`SessionLog`](super::SessionLog), which also checks it against the
/// logged sets.
///
/// Serializes as an object with `id`, `program_version_id`, `day`, `started_at`, `finished_at`
/// (`null` while in progress) and `status`. Unknown fields are ignored on load, for forward
/// compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    try_from = "SessionRepr<T>",
    bound(deserialize = "T: Deserialize<'de> + Ord + Copy")
)]
pub struct Session<T> {
    id: SessionId,
    program_version_id: ProgramVersionId,
    day: DayId,
    started_at: T,
    finished_at: Option<T>,
    status: SessionStatus,
}

/// The unchecked shape of a stored [`Session`].
#[derive(Deserialize)]
struct SessionRepr<T> {
    id: SessionId,
    program_version_id: ProgramVersionId,
    day: DayId,
    started_at: T,
    finished_at: Option<T>,
    status: SessionStatus,
}

impl<T: Ord + Copy> TryFrom<SessionRepr<T>> for Session<T> {
    type Error = SessionError;

    fn try_from(repr: SessionRepr<T>) -> Result<Self, Self::Error> {
        Self::from_parts(
            repr.id,
            repr.program_version_id,
            repr.day,
            repr.started_at,
            repr.status,
            repr.finished_at,
        )
    }
}

impl<T: Ord + Copy> Session<T> {
    /// Starts a new session now (`started_at`), in progress.
    #[must_use]
    pub const fn start(
        id: SessionId,
        program_version_id: ProgramVersionId,
        day: DayId,
        started_at: T,
    ) -> Self {
        Self {
            id,
            program_version_id,
            day,
            started_at,
            finished_at: None,
            status: SessionStatus::InProgress,
        }
    }

    /// Rebuilds a stored session (e.g. a database row), checking its invariants.
    ///
    /// # Errors
    /// - [`SessionError::InconsistentEnd`] when `finished_at` is set for an in-progress session, or
    ///   missing for an ended one.
    /// - [`SessionError::EndBeforeStart`] when `finished_at` is before `started_at`.
    pub fn from_parts(
        id: SessionId,
        program_version_id: ProgramVersionId,
        day: DayId,
        started_at: T,
        status: SessionStatus,
        finished_at: Option<T>,
    ) -> Result<Self, SessionError> {
        if status.is_ended() != finished_at.is_some() {
            return Err(SessionError::InconsistentEnd {
                session_id: id,
                status,
                has_end: finished_at.is_some(),
            });
        }
        if finished_at.is_some_and(|end| end < started_at) {
            return Err(SessionError::EndBeforeStart { session_id: id });
        }
        Ok(Self {
            id,
            program_version_id,
            day,
            started_at,
            finished_at,
            status,
        })
    }

    /// The client-generated session ID.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// The program version this session was run from.
    #[must_use]
    pub const fn program_version_id(&self) -> ProgramVersionId {
        self.program_version_id
    }

    /// The program day trained.
    #[must_use]
    pub const fn day(&self) -> &DayId {
        &self.day
    }

    /// When the session started.
    #[must_use]
    pub const fn started_at(&self) -> T {
        self.started_at
    }

    /// When the session ended, or `None` while it is in progress.
    #[must_use]
    pub const fn finished_at(&self) -> Option<T> {
        self.finished_at
    }

    /// The lifecycle status.
    #[must_use]
    pub const fn status(&self) -> SessionStatus {
        self.status
    }

    /// Whether the session has ended.
    #[must_use]
    pub const fn is_ended(&self) -> bool {
        self.status.is_ended()
    }

    /// Ends the session. Ending again with the same outcome and time is a no-op.
    ///
    /// Only checks the session's own invariants; [`SessionLog`](super::SessionLog) checks the sets.
    pub(super) fn end(&mut self, outcome: SessionOutcome, at: T) -> Result<Change, SessionError> {
        let status = SessionStatus::from(outcome);
        if self.status.is_ended() {
            return if self.status == status && self.finished_at == Some(at) {
                Ok(Change::Unchanged)
            } else {
                Err(SessionError::AlreadyEnded {
                    session_id: self.id,
                    status: self.status,
                })
            };
        }
        if at < self.started_at {
            return Err(SessionError::EndBeforeStart {
                session_id: self.id,
            });
        }
        self.status = status;
        self.finished_at = Some(at);
        Ok(Change::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn session_id() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(1))
    }

    fn version_id() -> ProgramVersionId {
        ProgramVersionId::from_uuid(Uuid::from_u128(2))
    }

    fn day() -> DayId {
        DayId::new("day-a").unwrap()
    }

    fn parts(
        status: SessionStatus,
        finished_at: Option<i64>,
    ) -> Result<Session<i64>, SessionError> {
        Session::from_parts(session_id(), version_id(), day(), 100, status, finished_at)
    }

    #[test]
    fn status_helpers() {
        assert!(!SessionStatus::InProgress.is_ended());
        assert!(SessionStatus::Completed.is_ended());
        assert!(SessionStatus::Skipped.is_ended());
        assert!(SessionStatus::Abandoned.is_ended());
        assert!(!SessionStatus::InProgress.advances_rotation());
        assert!(SessionStatus::Completed.advances_rotation());
        assert!(SessionStatus::Skipped.advances_rotation());
        assert!(!SessionStatus::Abandoned.advances_rotation());
    }

    #[test]
    fn status_display_and_serde() {
        let cases = [
            (SessionStatus::InProgress, "in progress", "\"in_progress\""),
            (SessionStatus::Completed, "completed", "\"completed\""),
            (SessionStatus::Skipped, "skipped", "\"skipped\""),
            (SessionStatus::Abandoned, "abandoned", "\"abandoned\""),
        ];
        for (status, text, json) in cases {
            assert_eq!(status.to_string(), text);
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
            assert_eq!(serde_json::from_str::<SessionStatus>(json).unwrap(), status);
        }
        assert!(serde_json::from_str::<SessionStatus>("\"done\"").is_err());
    }

    #[test]
    fn outcome_maps_to_status_and_serializes() {
        let cases = [
            (
                SessionOutcome::Completed,
                SessionStatus::Completed,
                "\"completed\"",
            ),
            (
                SessionOutcome::Skipped,
                SessionStatus::Skipped,
                "\"skipped\"",
            ),
            (
                SessionOutcome::Abandoned,
                SessionStatus::Abandoned,
                "\"abandoned\"",
            ),
        ];
        for (outcome, status, json) in cases {
            assert_eq!(SessionStatus::from(outcome), status);
            assert_eq!(serde_json::to_string(&outcome).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SessionOutcome>(json).unwrap(),
                outcome
            );
        }
        assert!(serde_json::from_str::<SessionOutcome>("\"in_progress\"").is_err());
    }

    #[test]
    fn start_is_in_progress() {
        let session = Session::start(session_id(), version_id(), day(), 100_i64);
        assert_eq!(session.id(), session_id());
        assert_eq!(session.program_version_id(), version_id());
        assert_eq!(session.day(), &day());
        assert_eq!(session.started_at(), 100);
        assert_eq!(session.finished_at(), None);
        assert_eq!(session.status(), SessionStatus::InProgress);
        assert!(!session.is_ended());
    }

    #[test]
    fn from_parts_accepts_consistent_sessions() {
        assert_eq!(
            parts(SessionStatus::InProgress, None).unwrap(),
            Session::start(session_id(), version_id(), day(), 100)
        );
        for status in [
            SessionStatus::Completed,
            SessionStatus::Skipped,
            SessionStatus::Abandoned,
        ] {
            // Ending at the very start is allowed (e.g. a day skipped straight away).
            for end in [100, 5_000] {
                let session = parts(status, Some(end)).unwrap();
                assert_eq!(session.status(), status);
                assert_eq!(session.finished_at(), Some(end));
                assert!(session.is_ended());
            }
        }
    }

    #[test]
    fn from_parts_rejects_inconsistent_end() {
        assert_eq!(
            parts(SessionStatus::InProgress, Some(200)),
            Err(SessionError::InconsistentEnd {
                session_id: session_id(),
                status: SessionStatus::InProgress,
                has_end: true
            })
        );
        let err = parts(SessionStatus::Completed, None).unwrap_err();
        assert_eq!(
            err,
            SessionError::InconsistentEnd {
                session_id: session_id(),
                status: SessionStatus::Completed,
                has_end: false
            }
        );
        assert!(
            err.to_string()
                .ends_with("is completed but its end time is missing")
        );
        let err = parts(SessionStatus::InProgress, Some(200)).unwrap_err();
        assert!(
            err.to_string()
                .ends_with("is in progress but its end time is set")
        );
    }

    #[test]
    fn from_parts_rejects_end_before_start() {
        assert_eq!(
            parts(SessionStatus::Completed, Some(99)),
            Err(SessionError::EndBeforeStart {
                session_id: session_id()
            })
        );
    }

    #[test]
    fn end_sets_status_and_time() {
        let mut session = Session::start(session_id(), version_id(), day(), 100_i64);
        assert_eq!(
            session.end(SessionOutcome::Skipped, 100).unwrap(),
            Change::Applied
        );
        assert_eq!(session.status(), SessionStatus::Skipped);
        assert_eq!(session.finished_at(), Some(100));
    }

    #[test]
    fn end_is_idempotent_and_rejects_a_different_second_end() {
        let mut session = Session::start(session_id(), version_id(), day(), 100_i64);
        assert_eq!(
            session.end(SessionOutcome::Completed, 500).unwrap(),
            Change::Applied
        );
        assert_eq!(
            session.end(SessionOutcome::Completed, 500).unwrap(),
            Change::Unchanged
        );
        let already = Err(SessionError::AlreadyEnded {
            session_id: session_id(),
            status: SessionStatus::Completed,
        });
        assert_eq!(session.end(SessionOutcome::Completed, 501), already);
        assert_eq!(session.end(SessionOutcome::Abandoned, 500), already);
        assert_eq!(session.finished_at(), Some(500));
        assert_eq!(session.status(), SessionStatus::Completed);
    }

    #[test]
    fn end_rejects_a_time_before_the_start() {
        let mut session = Session::start(session_id(), version_id(), day(), 100_i64);
        assert_eq!(
            session.end(SessionOutcome::Completed, 99),
            Err(SessionError::EndBeforeStart {
                session_id: session_id()
            })
        );
        assert!(!session.is_ended());
    }

    #[test]
    fn serde_round_trips_and_has_a_flat_shape() {
        let in_progress = Session::start(session_id(), version_id(), day(), 100_i64);
        let json = serde_json::to_value(&in_progress).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "program_version_id": "00000000-0000-0000-0000-000000000002",
                "day": "day-a",
                "started_at": 100,
                "finished_at": null,
                "status": "in_progress",
            })
        );
        assert_eq!(
            serde_json::from_value::<Session<i64>>(json).unwrap(),
            in_progress
        );

        let ended = parts(SessionStatus::Abandoned, Some(300)).unwrap();
        let text = serde_json::to_string(&ended).unwrap();
        assert_eq!(serde_json::from_str::<Session<i64>>(&text).unwrap(), ended);
    }

    #[test]
    fn deserialization_checks_invariants() {
        let base = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "program_version_id": "00000000-0000-0000-0000-000000000002",
            "day": "day-a",
            "started_at": 100,
        });
        let with = |finished_at: serde_json::Value, status: &str| {
            let mut value = base.clone();
            value["finished_at"] = finished_at;
            value["status"] = status.into();
            serde_json::from_value::<Session<i64>>(value)
        };
        let err = with(serde_json::Value::Null, "completed").unwrap_err();
        assert!(err.to_string().contains("end time is missing"), "{err}");
        let err = with(200.into(), "in_progress").unwrap_err();
        assert!(err.to_string().contains("end time is set"), "{err}");
        let err = with(50.into(), "completed").unwrap_err();
        assert!(
            err.to_string().contains("cannot end before it started"),
            "{err}"
        );
        let err = with(200.into(), "completed");
        assert!(err.is_ok());

        let mut bad_day = base.clone();
        bad_day["day"] = "Day A".into();
        bad_day["finished_at"] = serde_json::Value::Null;
        bad_day["status"] = "in_progress".into();
        assert!(serde_json::from_value::<Session<i64>>(bad_day).is_err());
    }
}
