//! Workout sessions (`workout_sessions`). Ids are generated on the client, and every write is
//! idempotent so that a retried request is harmless (#18, #25).
//!
//! A session can only reference one of its owner's program versions: the composite foreign key
//! `(program_version_id, user_id)` rejects anything else, which surfaces as
//! [`RepoError::NotFound`].

use sqlx::{
    PgPool,
    types::{Uuid, time::OffsetDateTime},
};

use super::{
    error::{Change, RepoError},
    ids::{ProgramId, ProgramVersionId, SessionId, UserId},
};

/// Where a session is in its lifecycle (the domain `SessionStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    InProgress,
    Completed,
    Skipped,
    Abandoned,
}

impl SessionStatus {
    /// The stored text, the same as the domain's serde name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Abandoned => "abandoned",
        }
    }

    fn parse(value: &str) -> Result<Self, RepoError> {
        match value {
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "skipped" => Ok(Self::Skipped),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(RepoError::Corrupt("workout_sessions.status")),
        }
    }
}

/// How a session ends (the domain `SessionOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    Completed,
    Skipped,
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

/// A session to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// Generated on the client.
    pub id: SessionId,
    pub program_version_id: ProgramVersionId,
    /// The program day slug (the domain `DayId`).
    pub day_id: String,
    pub started_at: OffsetDateTime,
}

/// A stored session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkoutSession {
    pub id: SessionId,
    pub program_version_id: ProgramVersionId,
    /// The program the version belongs to, for filtering history by program.
    pub program_id: ProgramId,
    pub day_id: String,
    pub status: SessionStatus,
    pub started_at: OffsetDateTime,
    /// Set exactly when the status is not in progress.
    pub finished_at: Option<OffsetDateTime>,
}

/// A `workout_sessions` row joined with its program version.
struct SessionRow {
    id: Uuid,
    program_version_id: Uuid,
    program_id: Uuid,
    day_id: String,
    status: String,
    started_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}

impl TryFrom<SessionRow> for WorkoutSession {
    type Error = RepoError;

    fn try_from(row: SessionRow) -> Result<Self, RepoError> {
        Ok(Self {
            id: SessionId::from_uuid(row.id),
            program_version_id: ProgramVersionId::from_uuid(row.program_version_id),
            program_id: ProgramId::from_uuid(row.program_id),
            day_id: row.day_id,
            status: SessionStatus::parse(&row.status)?,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
    }
}

/// Where a page of [`list`] starts: strictly before this session, in history order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub started_at: OffsetDateTime,
    pub id: SessionId,
}

impl WorkoutSession {
    /// The cursor for the page after this session.
    pub const fn cursor(&self) -> Cursor {
        Cursor {
            started_at: self.started_at,
            id: self.id,
        }
    }
}

/// Starts a session. Idempotent on the client id.
///
/// Returns [`Change::Applied`] when the session was created and [`Change::Unchanged`] when the user
/// already has this session with the same version, day and start time (whatever its status).
///
/// # Errors
/// - [`RepoError::Conflict`] when the user already has a session with this id and other values.
///   Another user's sessions play no part: ids are only unique per user.
/// - [`RepoError::SessionInProgress`] when the user has another session in progress.
/// - [`RepoError::NotFound`] when the program version is not one of the user's.
/// - [`RepoError::Invalid`] for a day id that is not a slug.
pub async fn start(pool: &PgPool, user: UserId, session: &NewSession) -> Result<Change, RepoError> {
    // Ids are unique per user (primary key `(user_id, id)`): another user's session with the same
    // id is a different row and never gets in the way.
    let result = sqlx::query!(
        "INSERT INTO workout_sessions (id, user_id, program_version_id, day_id, status, started_at)
         VALUES ($1, $2, $3, $4, 'in_progress', $5)
         ON CONFLICT (user_id, id) DO NOTHING",
        session.id.as_uuid(),
        user.as_uuid(),
        session.program_version_id.as_uuid(),
        session.day_id,
        session.started_at,
    )
    .execute(pool)
    .await;
    // Another session in progress: unless it is this very one (a concurrent duplicate of this
    // request can hit this index before the primary key), refuse.
    let in_progress = matches!(
        &result,
        Err(sqlx::Error::Database(error)) if error.constraint() == Some(ONE_IN_PROGRESS)
    );
    if !in_progress && result?.rows_affected() == 1 {
        return Ok(Change::Applied);
    }
    // This user already has a session with this id: compare with it.
    let same = sqlx::query_scalar!(
        r#"SELECT (program_version_id = $3 AND day_id = $4 AND started_at = $5) AS "same!"
           FROM workout_sessions WHERE id = $1 AND user_id = $2"#,
        session.id.as_uuid(),
        user.as_uuid(),
        session.program_version_id.as_uuid(),
        session.day_id,
        session.started_at,
    )
    .fetch_optional(pool)
    .await?;
    match same {
        Some(true) => Ok(Change::Unchanged),
        Some(false) => Err(RepoError::Conflict),
        None if in_progress => Err(RepoError::SessionInProgress),
        // Not inserted, yet no row of this user: cannot happen while sessions are never deleted
        // (except with their user). Nothing was saved, so a retry is the right answer.
        None => Err(RepoError::Transient),
    }
}

/// The partial unique index that allows one session in progress per user.
const ONE_IN_PROGRESS: &str = "workout_sessions_one_in_progress_idx";

/// Ends one of the user's in-progress sessions. Idempotent: ending it again with the same outcome
/// and time is [`Change::Unchanged`].
///
/// # Errors
/// - [`RepoError::NotFound`] when the user has no session with that id.
/// - [`RepoError::Conflict`] when it already ended with another outcome or time.
/// - [`RepoError::Invalid`] when `finished_at` is before the start.
pub async fn finish(
    pool: &PgPool,
    user: UserId,
    id: SessionId,
    outcome: SessionOutcome,
    finished_at: OffsetDateTime,
) -> Result<Change, RepoError> {
    let status = SessionStatus::from(outcome).as_str();
    let updated = sqlx::query!(
        "UPDATE workout_sessions SET status = $3, finished_at = $4
         WHERE id = $1 AND user_id = $2 AND status = 'in_progress'",
        id.as_uuid(),
        user.as_uuid(),
        status,
        finished_at,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 1 {
        return Ok(Change::Applied);
    }
    let same = sqlx::query_scalar!(
        r#"SELECT (status = $3 AND finished_at = $4) AS "same!"
           FROM workout_sessions WHERE id = $1 AND user_id = $2"#,
        id.as_uuid(),
        user.as_uuid(),
        status,
        finished_at,
    )
    .fetch_optional(pool)
    .await?;
    match same {
        None => Err(RepoError::NotFound),
        Some(true) => Ok(Change::Unchanged),
        Some(false) => Err(RepoError::Conflict),
    }
}

/// One of the user's sessions.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no session with that id.
pub async fn get(pool: &PgPool, user: UserId, id: SessionId) -> Result<WorkoutSession, RepoError> {
    let row = sqlx::query_as!(
        SessionRow,
        "SELECT s.id, s.program_version_id, v.program_id, s.day_id, s.status, s.started_at,
                s.finished_at
         FROM workout_sessions s JOIN program_versions v ON v.id = s.program_version_id
         WHERE s.id = $1 AND s.user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(RepoError::NotFound)?;
    WorkoutSession::try_from(row)
}

/// The user's most recently started session that is still in progress, if any.
pub async fn get_in_progress(
    pool: &PgPool,
    user: UserId,
) -> Result<Option<WorkoutSession>, RepoError> {
    let row = sqlx::query_as!(
        SessionRow,
        "SELECT s.id, s.program_version_id, v.program_id, s.day_id, s.status, s.started_at,
                s.finished_at
         FROM workout_sessions s JOIN program_versions v ON v.id = s.program_version_id
         WHERE s.user_id = $1 AND s.status = 'in_progress'
         ORDER BY s.started_at DESC, s.id DESC LIMIT 1",
        user.as_uuid(),
    )
    .fetch_optional(pool)
    .await?;
    row.map(WorkoutSession::try_from).transpose()
}

/// A page of the user's sessions, most recently started first.
///
/// `program` keeps only the sessions run from that program's versions (the rotation filter of
/// #18). `after` continues from a previous page's last session ([`WorkoutSession::cursor`]).
/// `limit` is clamped to 1..=100.
pub async fn list(
    pool: &PgPool,
    user: UserId,
    program: Option<ProgramId>,
    after: Option<Cursor>,
    limit: u32,
) -> Result<Vec<WorkoutSession>, RepoError> {
    let limit = i64::from(limit.clamp(1, 100));
    sqlx::query_as!(
        SessionRow,
        "SELECT s.id, s.program_version_id, v.program_id, s.day_id, s.status, s.started_at,
                s.finished_at
         FROM workout_sessions s JOIN program_versions v ON v.id = s.program_version_id
         WHERE s.user_id = $1
           AND ($2::uuid IS NULL OR v.program_id = $2)
           AND ($3::timestamptz IS NULL OR (s.started_at, s.id) < ($3, $4::uuid))
         ORDER BY s.started_at DESC, s.id DESC
         LIMIT $5",
        user.as_uuid(),
        program.map(ProgramId::as_uuid),
        after.map(|cursor| cursor.started_at),
        after.map(|cursor| cursor.id.as_uuid()),
        limit,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(WorkoutSession::try_from)
    .collect()
}

/// Every session run from any version of `program`, whatever its status, oldest first (by start,
/// then id). The input of the day rotation and of the progression history (#18); a user's history
/// in one program stays small (a few hundred sessions), so it is read in one go.
///
/// A program that is not the user's gives no sessions, like one that does not exist.
pub async fn list_in_program(
    pool: &PgPool,
    user: UserId,
    program: ProgramId,
) -> Result<Vec<WorkoutSession>, RepoError> {
    sqlx::query_as!(
        SessionRow,
        "SELECT s.id, s.program_version_id, v.program_id, s.day_id, s.status, s.started_at,
                s.finished_at
         FROM workout_sessions s
         JOIN program_versions v ON v.id = s.program_version_id AND v.user_id = s.user_id
         WHERE s.user_id = $1 AND v.program_id = $2
         ORDER BY s.started_at, s.id",
        user.as_uuid(),
        program.as_uuid(),
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(WorkoutSession::try_from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{
        MIGRATOR, programs,
        testing::{self, at, new_session, random_uuid},
    };

    #[test]
    fn status_text_round_trips_and_rejects_unknown_values() {
        for status in [
            SessionStatus::InProgress,
            SessionStatus::Completed,
            SessionStatus::Skipped,
            SessionStatus::Abandoned,
        ] {
            assert_eq!(SessionStatus::parse(status.as_str()).unwrap(), status);
        }
        assert!(matches!(
            SessionStatus::parse("paused"),
            Err(RepoError::Corrupt(_))
        ));
        assert_eq!(
            SessionStatus::from(SessionOutcome::Skipped),
            SessionStatus::Skipped
        );
        assert_eq!(
            SessionStatus::from(SessionOutcome::Completed),
            SessionStatus::Completed
        );
        assert_eq!(
            SessionStatus::from(SessionOutcome::Abandoned),
            SessionStatus::Abandoned
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn start_is_idempotent_and_detects_conflicts(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (program, version) = testing::program(&pool, user).await;
        let new = new_session(version);
        assert_eq!(start(&pool, user, &new).await.unwrap(), Change::Applied);
        assert_eq!(start(&pool, user, &new).await.unwrap(), Change::Unchanged);
        let stored = get(&pool, user, new.id).await.unwrap();
        assert_eq!(
            stored,
            WorkoutSession {
                id: new.id,
                program_version_id: version,
                program_id: program,
                day_id: "a".to_owned(),
                status: SessionStatus::InProgress,
                started_at: new.started_at,
                finished_at: None,
            }
        );
        for different in [
            NewSession {
                day_id: "b".to_owned(),
                ..new.clone()
            },
            NewSession {
                started_at: at(1),
                ..new.clone()
            },
            NewSession {
                program_version_id: testing::program(&pool, user).await.1,
                ..new.clone()
            },
        ] {
            let result = start(&pool, user, &different).await;
            assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
        }
        // Still a retry after the session ended.
        finish(&pool, user, new.id, SessionOutcome::Completed, at(10))
            .await
            .unwrap();
        assert_eq!(start(&pool, user, &new).await.unwrap(), Change::Unchanged);
        assert_eq!(
            get(&pool, user, new.id).await.unwrap().status,
            SessionStatus::Completed
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn concurrent_starts_create_one_session(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, version) = testing::program(&pool, user).await;
        let new = new_session(version);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (pool, new) = (pool.clone(), new.clone());
                tokio::spawn(async move { start(&pool, user, &new).await })
            })
            .collect();
        let mut applied = 0;
        for task in tasks {
            if task.await.unwrap().unwrap() == Change::Applied {
                applied += 1;
            }
        }
        assert_eq!(applied, 1);
        assert_eq!(list(&pool, user, None, None, 100).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn one_session_in_progress_per_user(pool: PgPool) {
        let (user, other_user) = testing::users_a_and_b(&pool).await;
        let (_, version) = testing::program(&pool, user).await;
        let first = new_session(version);
        start(&pool, user, &first).await.unwrap();
        let second = new_session(version);
        let result = start(&pool, user, &second).await;
        assert!(
            matches!(result, Err(RepoError::SessionInProgress)),
            "{result:?}"
        );
        // Retrying the one in progress is still fine; another user is not affected.
        assert_eq!(start(&pool, user, &first).await.unwrap(), Change::Unchanged);
        testing::session(&pool, other_user).await;
        // Once it ended, the next one can start.
        finish(&pool, user, first.id, SessionOutcome::Abandoned, at(1))
            .await
            .unwrap();
        assert_eq!(start(&pool, user, &second).await.unwrap(), Change::Applied);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn concurrent_starts_of_different_sessions_create_one(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, version) = testing::program(&pool, user).await;
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (pool, new) = (pool.clone(), new_session(version));
                tokio::spawn(async move { start(&pool, user, &new).await })
            })
            .collect();
        let mut applied = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(Change::Applied) => applied += 1,
                Err(RepoError::SessionInProgress) => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(applied, 1);
        assert_eq!(list(&pool, user, None, None, 100).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn start_rejects_invalid_days(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, version) = testing::program(&pool, user).await;
        let new = NewSession {
            day_id: "Day A".to_owned(),
            ..new_session(version)
        };
        let result = start(&pool, user, &new).await;
        assert!(
            matches!(result, Err(RepoError::Invalid { .. })),
            "{result:?}"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn finish_is_idempotent_and_detects_conflicts(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        let result = finish(&pool, user, session, SessionOutcome::Skipped, at(-1)).await;
        assert!(
            matches!(result, Err(RepoError::Invalid { .. })),
            "{result:?}"
        );
        assert_eq!(
            finish(&pool, user, session, SessionOutcome::Skipped, at(0))
                .await
                .unwrap(),
            Change::Applied
        );
        assert_eq!(
            finish(&pool, user, session, SessionOutcome::Skipped, at(0))
                .await
                .unwrap(),
            Change::Unchanged
        );
        for (outcome, time) in [
            (SessionOutcome::Completed, at(0)),
            (SessionOutcome::Skipped, at(5)),
        ] {
            let result = finish(&pool, user, session, outcome, time).await;
            assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
        }
        let stored = get(&pool, user, session).await.unwrap();
        assert_eq!(stored.status, SessionStatus::Skipped);
        assert_eq!(stored.finished_at, Some(at(0)));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn in_progress_and_history_pages(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (program_1, version_1) = testing::program(&pool, user).await;
        let (_, version_2) = testing::program(&pool, user).await;
        assert_eq!(get_in_progress(&pool, user).await.unwrap(), None);
        // Five sessions, one per hour; the last two from the second program.
        let mut ids = Vec::new();
        for hour in 0..5 {
            let new = NewSession {
                started_at: at(hour * 3_600),
                program_version_id: if hour < 3 { version_1 } else { version_2 },
                ..new_session(version_1)
            };
            start(&pool, user, &new).await.unwrap();
            if hour < 4 {
                finish(
                    &pool,
                    user,
                    new.id,
                    SessionOutcome::Completed,
                    at(hour * 3_600 + 60),
                )
                .await
                .unwrap();
            }
            ids.push(new.id);
        }
        assert_eq!(
            get_in_progress(&pool, user).await.unwrap().map(|s| s.id),
            Some(ids[4])
        );

        let first = list(&pool, user, None, None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ids[4], ids[3]]
        );
        let second = list(&pool, user, None, Some(first[1].cursor()), 2)
            .await
            .unwrap();
        assert_eq!(
            second.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ids[2], ids[1]]
        );
        let third = list(&pool, user, None, Some(second[1].cursor()), 2)
            .await
            .unwrap();
        assert_eq!(third.iter().map(|s| s.id).collect::<Vec<_>>(), vec![ids[0]]);
        assert!(
            list(&pool, user, None, Some(third[0].cursor()), 2)
                .await
                .unwrap()
                .is_empty()
        );

        let of_program_1 = list(&pool, user, Some(program_1), None, 100).await.unwrap();
        assert_eq!(
            of_program_1.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ids[2], ids[1], ids[0]]
        );
        // The limit is clamped to at least 1.
        assert_eq!(list(&pool, user, None, None, 0).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn list_in_program_keeps_every_version_and_status_oldest_first(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (program, version_1) = testing::program(&pool, user).await;
        let (_, version_2) = programs::add_version(&pool, user, program, &testing::document("v2"))
            .await
            .unwrap();
        let (_, other_version) = testing::program(&pool, user).await;
        let mut expected = Vec::new();
        for (hour, version) in [(2, version_2.id), (0, version_1), (1, other_version)] {
            let new = NewSession {
                started_at: at(hour * 3_600),
                program_version_id: version,
                ..new_session(version)
            };
            start(&pool, user, &new).await.unwrap();
            // Ended at once: one session in progress at a time.
            let outcome = if hour == 0 {
                SessionOutcome::Abandoned
            } else {
                SessionOutcome::Completed
            };
            finish(&pool, user, new.id, outcome, at(hour * 3_600 + 60))
                .await
                .unwrap();
            if version != other_version {
                expected.push((hour, new.id));
            }
        }
        expected.sort();
        let listed = list_in_program(&pool, user, program).await.unwrap();
        assert_eq!(
            listed.iter().map(|s| s.id).collect::<Vec<_>>(),
            expected.iter().map(|(_, id)| *id).collect::<Vec<_>>()
        );
        assert!(listed.iter().all(|s| s.program_id == program));
        assert_eq!(listed[0].status, SessionStatus::Abandoned);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn sessions_with_the_same_start_page_by_id(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, version) = testing::program(&pool, user).await;
        for _ in 0..3 {
            let new = new_session(version);
            start(&pool, user, &new).await.unwrap();
            finish(&pool, user, new.id, SessionOutcome::Completed, at(1))
                .await
                .unwrap();
        }
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = list(&pool, user, None, after, 1).await.unwrap();
            let Some(last) = page.last() else { break };
            after = Some(last.cursor());
            seen.extend(page.iter().map(|s| s.id));
        }
        assert_eq!(seen.len(), 3);
        let mut sorted = seen.clone();
        sorted.sort_by(|x, y| y.cmp(x));
        assert_eq!(seen, sorted);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn another_users_sessions_are_invisible_and_untouchable(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let (program_a, version_a) = testing::program(&pool, a).await;
        let session_a = new_session(version_a);
        start(&pool, a, &session_a).await.unwrap();
        let before = get(&pool, a, session_a.id).await.unwrap();
        let guessed = SessionId::from_uuid(random_uuid());

        for id in [session_a.id, guessed] {
            let result = get(&pool, b, id).await;
            assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
            let result = finish(&pool, b, id, SessionOutcome::Abandoned, at(10)).await;
            assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
        }
        assert_eq!(get_in_progress(&pool, b).await.unwrap(), None);
        assert!(list(&pool, b, None, None, 100).await.unwrap().is_empty());
        assert!(
            list_in_program(&pool, b, program_a)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            list(&pool, b, Some(program_a), None, 100)
                .await
                .unwrap()
                .is_empty()
        );

        // B cannot start a session from A's program version (same answer as an unknown version).
        let (_, version_b) = testing::program(&pool, b).await;
        for version in [version_a, ProgramVersionId::from_uuid(random_uuid())] {
            let result = start(&pool, b, &new_session(version)).await;
            assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
        }
        // A's exact session (A's version): B cannot use A's version, whatever the id.
        let result = start(&pool, b, &session_a).await;
        assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
        // A's session id with B's own version: B's own, independent session.
        let reuse = NewSession {
            program_version_id: version_b,
            ..session_a.clone()
        };
        assert_eq!(start(&pool, b, &reuse).await.unwrap(), Change::Applied);
        assert_eq!(start(&pool, b, &reuse).await.unwrap(), Change::Unchanged);
        assert_eq!(
            get(&pool, b, reuse.id).await.unwrap().program_version_id,
            version_b
        );
        finish(&pool, b, reuse.id, SessionOutcome::Abandoned, at(5))
            .await
            .unwrap();
        assert_eq!(get(&pool, a, session_a.id).await.unwrap(), before);
    }
}
