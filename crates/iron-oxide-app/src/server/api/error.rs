//! [`ApiError`]: how every server function fails (#68, conventions in `docs/api.md`).
//!
//! The client only ever sees an HTTP status and a short message that is safe to show. Details
//! (database errors, constraint names, ids) are logged server-side and never returned. A row that
//! does not exist and a row that belongs to another user are the same `404 Not found.`, so a
//! response never tells whether someone else's id exists.

use std::borrow::Cow;

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;
use iron_oxide_domain::{SessionError, ValueError, program::ProgramError};

use crate::server::{auth::AuthError, db::error::RepoError};

/// The public message of a 404.
pub const NOT_FOUND: &str = "Not found.";
/// The public message of a 503.
pub const TRANSIENT: &str = "The server is busy. Please try again.";
/// The public message of a 500.
pub const INTERNAL: &str = "Something went wrong. Please try again.";
/// The public message of a 401 (the same as sign-in's).
pub const UNAUTHORIZED: &str = "Please sign in.";

/// Why a server function failed. Converts into [`ServerFnError`] with `?`.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// `404`: no such row among the caller's own (it may not exist, or belong to someone else).
    #[error("not found")]
    NotFound,
    /// `409`: the request contradicts saved data (an id reused with different content, a session
    /// that has already ended). The message is shown to the user.
    #[error("conflict: {0}")]
    Conflict(Cow<'static, str>),
    /// `422`: invalid input. The message is shown to the user, so it must not contain anything the
    /// user did not send.
    #[error("invalid: {0}")]
    Invalid(Cow<'static, str>),
    /// `503`: nothing was saved and retrying the same request is safe and expected to succeed
    /// (a concurrent write got in the way, the database was briefly unreachable). The client retry
    /// queue (#30) retries it. The text is for the logs only.
    #[error("transient: {0}")]
    Transient(String),
    /// `401`: not signed in. [`AuthUser`](crate::server::auth::AuthUser) rejects the call before
    /// the body runs; this variant is for errors converted from [`AuthError`].
    #[error("not signed in")]
    Unauthorized,
    /// `403`: signed in, but the user's plan does not allow it (plan gating, #21). The message is
    /// shown to the user.
    #[error("forbidden: {0}")]
    Forbidden(Cow<'static, str>),
    /// `500`: a bug or an unexpected failure. The text is for the logs only.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    /// A `409` with a message for the user.
    #[must_use]
    pub fn conflict(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Conflict(message.into())
    }

    /// A `422` with a message for the user.
    #[must_use]
    pub fn invalid(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Invalid(message.into())
    }

    /// A `500`; `detail` is logged, never returned.
    #[must_use]
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        Self::Internal(detail.to_string())
    }

    /// The HTTP status and the message shown to the user.
    #[must_use]
    pub fn public(&self) -> (u16, &str) {
        match self {
            Self::NotFound => (404, NOT_FOUND),
            Self::Conflict(message) => (409, message),
            Self::Invalid(message) => (422, message),
            Self::Transient(_) => (503, TRANSIENT),
            Self::Unauthorized => (401, UNAUTHORIZED),
            Self::Forbidden(message) => (403, message),
            Self::Internal(_) => (500, INTERNAL),
        }
    }

    /// Logs the details at a level matching the cause.
    fn log(&self) {
        match self {
            Self::Internal(_) => tracing::error!(error = %self, "server function failed"),
            Self::Transient(_) => {
                tracing::warn!(error = %self, "server function failed, retryable")
            }
            Self::Unauthorized => tracing::debug!(error = %self, "server function refused"),
            _ => tracing::info!(error = %self, "server function refused"),
        }
    }
}

impl From<ApiError> for ServerFnError {
    fn from(error: ApiError) -> Self {
        error.log();
        let (code, message) = error.public();
        ServerFnError::ServerError {
            message: message.to_owned(),
            code,
            details: None,
        }
    }
}

impl From<RepoError> for ApiError {
    fn from(error: RepoError) -> Self {
        match error {
            RepoError::NotFound => Self::NotFound,
            RepoError::Conflict => Self::conflict("This was already saved with different values."),
            RepoError::SessionEnded => Self::conflict("This session has already ended."),
            RepoError::Transient => Self::Transient("concurrent write".to_owned()),
            RepoError::Invalid { constraint } => {
                tracing::info!(
                    constraint = constraint.as_deref().unwrap_or("(unnamed)"),
                    "database constraint rejected a value"
                );
                Self::invalid("Invalid value.")
            }
            RepoError::Corrupt(column) => Self::Internal(format!("corrupt column {column}")),
            RepoError::Database(source) if is_transient(&source) => {
                Self::Transient(source.to_string())
            }
            RepoError::Database(source) => Self::Internal(source.to_string()),
        }
    }
}

/// Database failures that a retry can fix: the pool or the connection was unavailable (a Neon
/// compute waking up, a restart), or Postgres aborted the statement because of a concurrent one.
fn is_transient(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => true,
        sqlx::Error::Database(db_error) => {
            // 40001 serialization_failure, 40P01 deadlock_detected, 57P01 admin_shutdown.
            matches!(
                db_error.code().as_deref(),
                Some("40001" | "40P01" | "57P01")
            )
        }
        _ => false,
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        let (code, message) = error.public();
        match code {
            401 => Self::Unauthorized,
            404 => Self::NotFound,
            409 => Self::conflict(message.to_owned()),
            400 | 422 => Self::invalid(message.to_owned()),
            _ => Self::Internal(error.to_string()),
        }
    }
}

impl From<ValueError> for ApiError {
    /// A value the client sent is out of range or malformed. The message only repeats what the
    /// user sent.
    fn from(error: ValueError) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<ProgramError> for ApiError {
    /// A program document the user sent is not valid. The messages point at the offending field
    /// and cap how much of the user's text they echo.
    fn from(error: ProgramError) -> Self {
        Self::invalid(error.to_string())
    }
}

impl From<SessionError> for ApiError {
    /// Fixed messages: the domain's own text names session and set ids.
    fn from(error: SessionError) -> Self {
        let message = match &error {
            SessionError::AlreadyEnded { .. } => "This session has already ended.",
            SessionError::SetConflict { .. } => "This set was already saved with different values.",
            SessionError::DuplicateSetId { .. } => "The same set appears more than once.",
            SessionError::SetBeforeStart { .. } => {
                "A set cannot be completed before its session started."
            }
            SessionError::EndBeforeStart { .. } | SessionError::EndBeforeSet { .. } => {
                "A session cannot end before its sets were completed."
            }
            // Only a stored session can be inconsistent: the data, not the request, is wrong.
            SessionError::InconsistentEnd { .. } => return Self::Internal(error.to_string()),
        };
        match error {
            SessionError::AlreadyEnded { .. } | SessionError::SetConflict { .. } => {
                Self::conflict(message)
            }
            _ => Self::invalid(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_oxide_domain::{SessionId, SessionStatus, SetId};
    use uuid::Uuid;

    fn server_error(error: impl Into<ApiError>) -> (u16, String) {
        match ServerFnError::from(error.into()) {
            ServerFnError::ServerError {
                message,
                code,
                details: None,
            } => (code, message),
            other => panic!("not a server error without details: {other:?}"),
        }
    }

    #[test]
    fn every_variant_has_its_status_and_message() {
        let cases = [
            (ApiError::NotFound, 404, NOT_FOUND),
            (ApiError::conflict("Taken."), 409, "Taken."),
            (ApiError::invalid("Too heavy."), 422, "Too heavy."),
            (ApiError::Transient("x".to_owned()), 503, TRANSIENT),
            (ApiError::Unauthorized, 401, UNAUTHORIZED),
            (ApiError::Forbidden("Pro only.".into()), 403, "Pro only."),
            (ApiError::internal("x"), 500, INTERNAL),
        ];
        for (error, code, message) in cases {
            assert_eq!(server_error(error), (code, message.to_owned()));
        }
    }

    #[test]
    fn repository_errors_map_to_statuses() {
        let status = |error: RepoError| server_error(error).0;
        assert_eq!(status(RepoError::NotFound), 404);
        assert_eq!(status(RepoError::Conflict), 409);
        assert_eq!(status(RepoError::SessionEnded), 409);
        assert_eq!(status(RepoError::Transient), 503);
        assert_eq!(status(RepoError::Invalid { constraint: None }), 422);
        assert_eq!(status(RepoError::Corrupt("reps")), 500);
        assert_eq!(status(RepoError::Database(sqlx::Error::RowNotFound)), 500);
        assert_eq!(status(RepoError::Database(sqlx::Error::PoolTimedOut)), 503);
        assert_eq!(status(RepoError::Database(sqlx::Error::PoolClosed)), 503);
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        assert_eq!(status(RepoError::Database(sqlx::Error::Io(io))), 503);
    }

    #[test]
    fn internal_details_never_reach_the_client() {
        let secret = "relation \"workout_sets\" does not exist at 10.0.0.3";
        let errors: Vec<ApiError> = vec![
            ApiError::internal(secret),
            ApiError::Transient(secret.to_owned()),
            RepoError::Database(sqlx::Error::Protocol(secret.to_owned())).into(),
            RepoError::Invalid {
                constraint: Some("workout_sets_reps_check".to_owned()),
            }
            .into(),
            RepoError::Corrupt("workout_sets.reps").into(),
            AuthError::Database(sqlx::Error::Protocol(secret.to_owned())).into(),
            AuthError::Internal(secret.to_owned()).into(),
        ];
        for error in errors {
            let (_, message) = server_error(error);
            assert!(!message.contains("10.0.0.3"), "{message}");
            assert!(!message.contains("workout_sets"), "{message}");
        }
    }

    #[test]
    fn session_errors_never_echo_ids() {
        let session_id = SessionId::from_uuid(Uuid::from_u128(0xabc));
        let set_id = SetId::from_uuid(Uuid::from_u128(0xdef));
        let cases = [
            (
                SessionError::AlreadyEnded {
                    session_id,
                    status: SessionStatus::Completed,
                },
                409,
            ),
            (SessionError::SetConflict { set_id }, 409),
            (SessionError::DuplicateSetId { set_id }, 422),
            (SessionError::SetBeforeStart { set_id }, 422),
            (SessionError::EndBeforeStart { session_id }, 422),
            (SessionError::EndBeforeSet { session_id, set_id }, 422),
            (
                SessionError::InconsistentEnd {
                    session_id,
                    status: SessionStatus::Completed,
                    has_end: false,
                },
                500,
            ),
        ];
        for (error, code) in cases {
            let (actual, message) = server_error(error);
            assert_eq!(actual, code, "{message}");
            assert!(
                !message.contains("abc") && !message.contains("def"),
                "{message}"
            );
        }
    }

    #[test]
    fn auth_errors_keep_their_status() {
        let status = |error: AuthError| server_error(error).0;
        assert_eq!(status(AuthError::Unauthenticated), 401);
        assert_eq!(status(AuthError::NotFound), 404);
        assert_eq!(status(AuthError::LastSignInMethod), 409);
        assert_eq!(status(AuthError::Invalid("Too long.".to_owned())), 422);
        assert_eq!(status(AuthError::Internal("x".to_owned())), 500);
        assert_eq!(
            server_error(AuthError::Invalid("Too long.".to_owned())).1,
            "Too long."
        );
    }

    #[test]
    fn value_and_program_errors_are_invalid_with_their_message() {
        let error = "x".parse::<SessionId>().unwrap_err();
        let (code, message) = server_error(error);
        assert_eq!(code, 422);
        assert!(message.starts_with("invalid session id"), "{message}");

        let program = iron_oxide_domain::program::Program::from_json("{").unwrap_err();
        assert_eq!(server_error(program).0, 422);
    }
}
