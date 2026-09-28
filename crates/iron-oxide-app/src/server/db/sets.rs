//! Logged sets (`workout_sets`). Ids are generated on the client, so a retried save is recognised
//! as the same set instead of creating a duplicate (#18, #25).
//!
//! A set can only belong to one of its owner's sessions: the composite foreign key
//! `(session_id, user_id)` makes pointing at another user's session impossible in the database.

use sqlx::{
    PgPool,
    types::{Uuid, time::OffsetDateTime},
};

use super::{
    error::{Change, RepoError, narrow},
    ids::{SessionId, SetId, UserId},
};

/// A logged set (the domain `LoggedSet`), as saved and as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedSet {
    /// Generated on the client: the idempotency key.
    pub id: SetId,
    pub session_id: SessionId,
    /// The exercise slug (the domain `ExerciseId`).
    pub exercise_id: String,
    /// Position among the exercise's sets of the same kind in the session, from 0.
    pub set_index: u16,
    pub reps: u16,
    /// Load in nanograms (the domain `Weight`), `None` for body-weight work.
    pub weight_ng: Option<u64>,
    /// Time under tension in seconds (the domain `Seconds`), `None` when not timed.
    pub duration_s: Option<u32>,
    pub warmup: bool,
    pub completed_at: OffsetDateTime,
}

/// A `workout_sets` row.
struct SetRow {
    id: Uuid,
    session_id: Uuid,
    exercise_id: String,
    set_index: i32,
    reps: i32,
    weight_ng: Option<i64>,
    duration_s: Option<i64>,
    warmup: bool,
    completed_at: OffsetDateTime,
}

impl TryFrom<SetRow> for LoggedSet {
    type Error = RepoError;

    fn try_from(row: SetRow) -> Result<Self, RepoError> {
        Ok(Self {
            id: SetId::from_uuid(row.id),
            session_id: SessionId::from_uuid(row.session_id),
            exercise_id: row.exercise_id,
            set_index: narrow(row.set_index.into(), "workout_sets.set_index")?,
            reps: narrow(row.reps.into(), "workout_sets.reps")?,
            weight_ng: row
                .weight_ng
                .map(|weight| narrow(weight, "workout_sets.weight_ng"))
                .transpose()?,
            duration_s: row
                .duration_s
                .map(|duration| narrow(duration, "workout_sets.duration_s"))
                .transpose()?,
            warmup: row.warmup,
            completed_at: row.completed_at,
        })
    }
}

/// Saves a set in one of the user's in-progress sessions. Idempotent on the set id.
///
/// Returns [`Change::Applied`] when the set was saved and [`Change::Unchanged`] when the user
/// already has this exact set (even if the session has ended since, so a queued retry succeeds).
/// Another user's row is never read back, compared or modified.
///
/// # Errors
/// - [`RepoError::Conflict`] when the id is already used by a different set, or by a set the user
///   cannot see.
/// - [`RepoError::NotFound`] when the session is not one of the user's.
/// - [`RepoError::SessionEnded`] when the set is new and the session has ended.
/// - [`RepoError::Invalid`] for an exercise id that is not a slug or a weight above 2000 kg.
pub async fn upsert_idempotent(
    pool: &PgPool,
    user: UserId,
    set: &LoggedSet,
) -> Result<Change, RepoError> {
    let weight_ng = set
        .weight_ng
        .map(|weight| {
            i64::try_from(weight).map_err(|_| RepoError::Invalid {
                constraint: Some("workout_sets_weight_ng_check".to_owned()),
            })
        })
        .transpose()?;
    let duration_s = set.duration_s.map(i64::from);

    // Inserts only into a session of this user that is still in progress. A taken id (whoever
    // owns it) inserts nothing.
    let inserted = sqlx::query!(
        "INSERT INTO workout_sets
             (id, session_id, user_id, exercise_id, set_index, reps, weight_ng, duration_s, warmup,
              completed_at)
         SELECT $1, s.id, s.user_id, $4, $5, $6, $7, $8, $9, $10
         FROM workout_sessions s
         WHERE s.id = $2 AND s.user_id = $3 AND s.status = 'in_progress'
         ON CONFLICT (id) DO NOTHING",
        set.id.as_uuid(),
        set.session_id.as_uuid(),
        user.as_uuid(),
        set.exercise_id,
        i32::from(set.set_index),
        i32::from(set.reps),
        weight_ng,
        duration_s,
        set.warmup,
        set.completed_at,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if inserted == 1 {
        return Ok(Change::Applied);
    }

    // Nothing inserted: the id is taken, or the session is not an in-progress one of this user.
    // Only this user's own set is compared.
    let same = sqlx::query_scalar!(
        r#"SELECT (session_id = $3 AND exercise_id = $4 AND set_index = $5 AND reps = $6
                   AND weight_ng IS NOT DISTINCT FROM $7 AND duration_s IS NOT DISTINCT FROM $8
                   AND warmup = $9 AND completed_at = $10) AS "same!"
           FROM workout_sets WHERE id = $1 AND user_id = $2"#,
        set.id.as_uuid(),
        user.as_uuid(),
        set.session_id.as_uuid(),
        set.exercise_id,
        i32::from(set.set_index),
        i32::from(set.reps),
        weight_ng,
        duration_s,
        set.warmup,
        set.completed_at,
    )
    .fetch_optional(pool)
    .await?;
    match same {
        Some(true) => return Ok(Change::Unchanged),
        Some(false) => return Err(RepoError::Conflict),
        None => {}
    }

    // Not this user's set. Why the insert did nothing depends only on this user's session.
    let status = sqlx::query_scalar!(
        "SELECT status FROM workout_sessions WHERE id = $1 AND user_id = $2",
        set.session_id.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(pool)
    .await?;
    match status.as_deref() {
        None => Err(RepoError::NotFound),
        // The session accepts sets, so the id must belong to a set this user cannot see.
        Some("in_progress") => Err(RepoError::Conflict),
        Some(_) => Err(RepoError::SessionEnded),
    }
}

/// The sets of one of the user's sessions, in the order they were completed.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no session with that id.
pub async fn list_for_session(
    pool: &PgPool,
    user: UserId,
    session: SessionId,
) -> Result<Vec<LoggedSet>, RepoError> {
    let mut tx = pool.begin().await?;
    let owned = sqlx::query_scalar!(
        r#"SELECT true AS "owned!" FROM workout_sessions WHERE id = $1 AND user_id = $2"#,
        session.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    if owned.is_none() {
        return Err(RepoError::NotFound);
    }
    let sets = sqlx::query_as!(
        SetRow,
        "SELECT id, session_id, exercise_id, set_index, reps, weight_ng, duration_s, warmup,
                completed_at
         FROM workout_sets WHERE session_id = $1 AND user_id = $2
         ORDER BY completed_at, id",
        session.as_uuid(),
        user.as_uuid(),
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(LoggedSet::try_from)
    .collect::<Result<Vec<_>, RepoError>>()?;
    tx.commit().await?;
    Ok(sets)
}
