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
/// - [`RepoError::Conflict`] when the id is already used with other values, or by a session the
///   user cannot see.
/// - [`RepoError::NotFound`] when the program version is not one of the user's.
/// - [`RepoError::Invalid`] for a day id that is not a slug.
pub async fn start(pool: &PgPool, user: UserId, session: &NewSession) -> Result<Change, RepoError> {
    let inserted = sqlx::query!(
        "INSERT INTO workout_sessions (id, user_id, program_version_id, day_id, status, started_at)
         VALUES ($1, $2, $3, $4, 'in_progress', $5)
         ON CONFLICT (id) DO NOTHING",
        session.id.as_uuid(),
        user.as_uuid(),
        session.program_version_id.as_uuid(),
        session.day_id,
        session.started_at,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if inserted == 1 {
        return Ok(Change::Applied);
    }
    // The id exists. Only a session of this user is ever compared or reported on.
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
        Some(false) | None => Err(RepoError::Conflict),
    }
}

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
