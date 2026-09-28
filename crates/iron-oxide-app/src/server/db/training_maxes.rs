//! Training maxes (`training_maxes`): per user and per exercise, used by percentage-based loads.
//!
//! Each one carries `set_at`, when the lifter entered it. The progression engine (#57) starts from
//! the training max and replays the history after `set_at`
//! ([`sets::completed_for_exercise`](super::sets::completed_for_exercise)), so it must never be
//! stored back as a training max without `set_at` moving to the time it was computed for:
//! otherwise the same sets would be counted twice. [`set`] always writes both together.

use sqlx::{PgPool, types::time::OffsetDateTime};

use super::{
    error::{RepoError, narrow},
    ids::UserId,
};

/// One exercise's training max.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainingMax {
    /// The exercise slug (the domain `ExerciseId`).
    pub exercise_id: String,
    /// In nanograms (the domain `Weight`).
    pub weight_ng: u64,
    /// When the lifter entered it: the anchor of the progression replay.
    pub set_at: OffsetDateTime,
}

/// The user's training maxes, by exercise id.
pub async fn list(pool: &PgPool, user: UserId) -> Result<Vec<TrainingMax>, RepoError> {
    sqlx::query!(
        "SELECT exercise_id, weight_ng, set_at FROM training_maxes
         WHERE user_id = $1 ORDER BY exercise_id",
        user.as_uuid()
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(TrainingMax {
            exercise_id: row.exercise_id,
            weight_ng: narrow(row.weight_ng, "training_maxes.weight_ng")?,
            set_at: row.set_at,
        })
    })
    .collect()
}

/// Sets (or replaces) the user's training max for an exercise, with its `set_at` anchor.
///
/// # Errors
/// [`RepoError::Invalid`] for an exercise id that is not a slug or a weight above 2000 kg.
pub async fn set(pool: &PgPool, user: UserId, max: &TrainingMax) -> Result<(), RepoError> {
    let weight_ng = i64::try_from(max.weight_ng).map_err(|_| RepoError::Invalid {
        constraint: Some("training_maxes_weight_ng_check".to_owned()),
    })?;
    sqlx::query!(
        "INSERT INTO training_maxes (user_id, exercise_id, weight_ng, set_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, exercise_id)
         DO UPDATE SET weight_ng = EXCLUDED.weight_ng, set_at = EXCLUDED.set_at",
        user.as_uuid(),
        max.exercise_id,
        weight_ng,
        max.set_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes the user's training max for an exercise.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has none for that exercise.
pub async fn delete(pool: &PgPool, user: UserId, exercise_id: &str) -> Result<(), RepoError> {
    let deleted = sqlx::query!(
        "DELETE FROM training_maxes WHERE user_id = $1 AND exercise_id = $2",
        user.as_uuid(),
        exercise_id,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(RepoError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{
        MIGRATOR,
        testing::{self, at},
    };

    fn squat(weight_ng: u64) -> TrainingMax {
        TrainingMax {
            exercise_id: "back-squat".to_owned(),
            weight_ng,
            set_at: at(0),
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn set_replaces_and_delete_removes(pool: PgPool) {
        let user = testing::user(&pool).await;
        assert!(list(&pool, user).await.unwrap().is_empty());
        set(&pool, user, &squat(100_000_000_000_000)).await.unwrap();
        // Replacing moves the anchor too.
        let replaced = TrainingMax {
            set_at: at(3_600),
            ..squat(0)
        };
        set(&pool, user, &replaced).await.unwrap();
        let bench = TrainingMax {
            exercise_id: "bench".to_owned(),
            weight_ng: 2_000_000_000_000_000,
            set_at: at(-5),
        };
        set(&pool, user, &bench).await.unwrap();
        assert_eq!(list(&pool, user).await.unwrap(), vec![replaced, bench]);
        delete(&pool, user, "back-squat").await.unwrap();
        assert!(matches!(
            delete(&pool, user, "back-squat").await,
            Err(RepoError::NotFound)
        ));
        assert_eq!(list(&pool, user).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn invalid_values_are_rejected(pool: PgPool) {
        let user = testing::user(&pool).await;
        for bad in [
            squat(2_000_000_000_000_001),
            squat(u64::MAX),
            TrainingMax {
                exercise_id: "Back Squat".to_owned(),
                ..squat(1)
            },
            TrainingMax {
                exercise_id: "a".repeat(65),
                ..squat(1)
            },
        ] {
            let error = set(&pool, user, &bad).await.unwrap_err();
            assert!(matches!(error, RepoError::Invalid { .. }), "{error:?}");
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn users_only_see_and_change_their_own_training_maxes(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        set(&pool, a, &squat(100)).await.unwrap();
        assert!(list(&pool, b).await.unwrap().is_empty());
        // B deleting the same exercise: not found, and A's row stays.
        assert!(matches!(
            delete(&pool, b, "back-squat").await,
            Err(RepoError::NotFound)
        ));
        // B setting the same exercise creates B's own row.
        set(&pool, b, &squat(200)).await.unwrap();
        assert_eq!(list(&pool, a).await.unwrap(), vec![squat(100)]);
        assert_eq!(list(&pool, b).await.unwrap(), vec![squat(200)]);
    }
}
