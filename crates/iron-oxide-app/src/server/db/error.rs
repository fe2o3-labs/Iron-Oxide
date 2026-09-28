//! The repository error type.

use sqlx::error::ErrorKind;

/// Why a repository call failed.
///
/// The messages never contain ids, values or database error text, so a server function may show
/// them to the user as they are. In particular, a row that does not exist and a row that belongs to
/// another user both give [`RepoError::NotFound`]: a caller cannot learn whether someone else's id
/// exists.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// No such row among the caller's own rows (it may not exist, or belong to someone else).
    #[error("not found")]
    NotFound,
    /// The id is already used with different content (a retried write that is not a retry), or,
    /// for client-generated ids, by a row the caller cannot see.
    #[error("conflicts with data that is already saved")]
    Conflict,
    /// The session has ended: no new set can be added to it.
    #[error("the session has already ended")]
    SessionEnded,
    /// A value broke a database constraint (range, format, immutability).
    #[error("invalid value")]
    Invalid {
        /// The constraint that rejected it, when Postgres names one (for logs, not for users).
        constraint: Option<String>,
    },
    /// A stored value does not fit its Rust type. Means the schema and the code disagree.
    #[error("stored data is invalid")]
    Corrupt(&'static str),
    /// Any other database failure (connection, timeout, ...). Log the source, never show it.
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

impl From<sqlx::Error> for RepoError {
    /// Maps constraint violations to the variants above. A foreign key violation means a referenced
    /// row is not among the caller's (the composite keys include `user_id`), hence `NotFound`.
    fn from(error: sqlx::Error) -> Self {
        let sqlx::Error::Database(db_error) = &error else {
            return Self::Database(error);
        };
        let constraint = db_error.constraint().map(str::to_owned);
        match db_error.kind() {
            ErrorKind::ForeignKeyViolation => Self::NotFound,
            ErrorKind::UniqueViolation => Self::Conflict,
            ErrorKind::CheckViolation | ErrorKind::NotNullViolation => Self::Invalid { constraint },
            // 23000 integrity_constraint_violation: raised by the immutability triggers.
            _ if db_error.code().as_deref() == Some("23000") => Self::Invalid { constraint },
            _ => Self::Database(error),
        }
    }
}

/// The result of an idempotent write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The row was written.
    Applied,
    /// The exact same write had already been applied: nothing changed.
    Unchanged,
}

/// Converts a stored integer to a narrower Rust type, or reports the column as corrupt.
pub(super) fn narrow<T: TryFrom<i64>>(value: i64, column: &'static str) -> Result<T, RepoError> {
    T::try_from(value).map_err(|_| RepoError::Corrupt(column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_generic() {
        assert_eq!(RepoError::NotFound.to_string(), "not found");
        assert_eq!(
            RepoError::Invalid {
                constraint: Some("workout_sets_reps_check".to_owned())
            }
            .to_string(),
            "invalid value"
        );
        assert_eq!(
            RepoError::Database(sqlx::Error::PoolTimedOut).to_string(),
            "database error"
        );
    }

    #[test]
    fn non_database_errors_stay_database_errors() {
        assert!(matches!(
            RepoError::from(sqlx::Error::RowNotFound),
            RepoError::Database(sqlx::Error::RowNotFound)
        ));
    }

    #[test]
    fn narrow_rejects_out_of_range_values() {
        assert_eq!(narrow::<u16>(65_535, "reps").unwrap(), u16::MAX);
        assert!(matches!(
            narrow::<u16>(65_536, "reps"),
            Err(RepoError::Corrupt("reps"))
        ));
        assert!(matches!(
            narrow::<u64>(-1, "weight_ng"),
            Err(RepoError::Corrupt("weight_ng"))
        ));
    }
}
