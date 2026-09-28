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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{
        MIGRATOR,
        sessions::{self, SessionOutcome},
        testing::{self, at, new_set, random_uuid},
    };

    fn assert_err<T: std::fmt::Debug>(result: Result<T, RepoError>, expected: &str) {
        let matches = matches!(
            (&result, expected),
            (Err(RepoError::NotFound), "not found")
                | (Err(RepoError::Conflict), "conflict")
                | (Err(RepoError::SessionEnded), "ended")
                | (Err(RepoError::Invalid { .. }), "invalid")
        );
        assert!(matches, "expected {expected}, got {result:?}");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_set_round_trips_with_every_field(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        let heavy = LoggedSet {
            set_index: u16::MAX,
            reps: u16::MAX,
            weight_ng: Some(2_000_000_000_000_000),
            duration_s: Some(u32::MAX),
            warmup: true,
            completed_at: at(1),
            ..new_set(session)
        };
        let bodyweight = LoggedSet {
            set_index: 0,
            reps: 0,
            weight_ng: None,
            duration_s: None,
            completed_at: at(2),
            ..new_set(session)
        };
        for set in [&heavy, &bodyweight] {
            assert_eq!(
                upsert_idempotent(&pool, user, set).await.unwrap(),
                Change::Applied
            );
        }
        assert_eq!(
            list_for_session(&pool, user, session).await.unwrap(),
            vec![heavy, bodyweight]
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn same_id_and_content_is_one_row_and_different_content_conflicts(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        let set = new_set(session);
        assert_eq!(
            upsert_idempotent(&pool, user, &set).await.unwrap(),
            Change::Applied
        );
        for _ in 0..3 {
            assert_eq!(
                upsert_idempotent(&pool, user, &set).await.unwrap(),
                Change::Unchanged
            );
        }
        let other_session = testing::session(&pool, user).await;
        for different in [
            LoggedSet {
                reps: 4,
                ..set.clone()
            },
            LoggedSet {
                set_index: 1,
                ..set.clone()
            },
            LoggedSet {
                weight_ng: None,
                ..set.clone()
            },
            LoggedSet {
                weight_ng: Some(1),
                ..set.clone()
            },
            LoggedSet {
                duration_s: Some(30),
                ..set.clone()
            },
            LoggedSet {
                warmup: true,
                ..set.clone()
            },
            LoggedSet {
                exercise_id: "bench".to_owned(),
                ..set.clone()
            },
            LoggedSet {
                completed_at: at(61),
                ..set.clone()
            },
            LoggedSet {
                session_id: other_session,
                ..set.clone()
            },
        ] {
            assert_err(upsert_idempotent(&pool, user, &different).await, "conflict");
        }
        assert_eq!(
            list_for_session(&pool, user, session).await.unwrap(),
            vec![set]
        );
        assert!(
            list_for_session(&pool, user, other_session)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn concurrent_duplicate_saves_create_one_row(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        let set = new_set(session);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (pool, set) = (pool.clone(), set.clone());
                tokio::spawn(async move { upsert_idempotent(&pool, user, &set).await })
            })
            .collect();
        let mut applied = 0;
        for task in tasks {
            if task.await.unwrap().unwrap() == Change::Applied {
                applied += 1;
            }
        }
        assert_eq!(applied, 1);
        assert_eq!(
            list_for_session(&pool, user, session).await.unwrap(),
            vec![set]
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn an_ended_session_takes_retries_but_no_new_sets(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        let logged = testing::set(&pool, user, session).await;
        sessions::finish(&pool, user, session, SessionOutcome::Completed, at(120))
            .await
            .unwrap();
        assert_eq!(
            upsert_idempotent(&pool, user, &logged).await.unwrap(),
            Change::Unchanged
        );
        assert_err(
            upsert_idempotent(&pool, user, &new_set(session)).await,
            "ended",
        );
        assert_err(
            upsert_idempotent(
                &pool,
                user,
                &LoggedSet {
                    reps: 1,
                    ..logged.clone()
                },
            )
            .await,
            "conflict",
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn invalid_values_are_rejected(pool: PgPool) {
        let user = testing::user(&pool).await;
        let session = testing::session(&pool, user).await;
        for bad in [
            LoggedSet {
                weight_ng: Some(2_000_000_000_000_001),
                ..new_set(session)
            },
            LoggedSet {
                weight_ng: Some(u64::MAX),
                ..new_set(session)
            },
            LoggedSet {
                exercise_id: "Back squat".to_owned(),
                ..new_set(session)
            },
            LoggedSet {
                exercise_id: String::new(),
                ..new_set(session)
            },
        ] {
            assert_err(upsert_idempotent(&pool, user, &bad).await, "invalid");
        }
        assert!(
            list_for_session(&pool, user, session)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn another_users_sets_are_invisible_and_untouchable(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let session_a = testing::session(&pool, a).await;
        let set_a = testing::set(&pool, a, session_a).await;
        let session_b = testing::session(&pool, b).await;
        let guessed_session = SessionId::from_uuid(random_uuid());

        // Reading A's session's sets: the same answer as for a session that does not exist.
        for session in [session_a, guessed_session] {
            assert_err(list_for_session(&pool, b, session).await, "not found");
        }
        // Logging into A's session: the same answer as into a session that does not exist, even
        // with A's exact set.
        for session in [session_a, guessed_session] {
            assert_err(
                upsert_idempotent(&pool, b, &new_set(session)).await,
                "not found",
            );
        }
        assert_err(upsert_idempotent(&pool, b, &set_a).await, "not found");
        // Reusing A's set id in B's own session: rejected, and A's set is not returned or compared
        // (identical or not, the answer is the same).
        let reused = LoggedSet {
            session_id: session_b,
            ..set_a.clone()
        };
        assert_err(upsert_idempotent(&pool, b, &reused).await, "conflict");
        let reused = LoggedSet { reps: 1, ..reused };
        assert_err(upsert_idempotent(&pool, b, &reused).await, "conflict");

        assert!(
            list_for_session(&pool, b, session_b)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            list_for_session(&pool, a, session_a).await.unwrap(),
            vec![set_a]
        );
    }
}
