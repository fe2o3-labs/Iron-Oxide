//! The program a user trains with (`active_program`).
//!
//! Only one of the user's own programs can be active: a composite foreign key on
//! `(program_id, user_id)` enforces it in the database, so a built-in (copy it first) or another
//! user's program never can be, whatever the caller passes.

use sqlx::PgPool;

use super::{
    error::RepoError,
    ids::{ProgramId, UserId},
};

/// The user's active program, or `None` if they have not chosen one.
pub async fn get(pool: &PgPool, user: UserId) -> Result<Option<ProgramId>, RepoError> {
    let program = sqlx::query_scalar!(
        "SELECT program_id FROM active_program WHERE user_id = $1",
        user.as_uuid()
    )
    .fetch_optional(pool)
    .await?;
    Ok(program.map(ProgramId::from_uuid))
}

/// Makes one of the user's programs the active one.
///
/// The program's row is locked while it is checked and set, like `programs::set_archived` does,
/// so a concurrent archive of the same program either happens first (and this is refused) or
/// waits and then sees the program is active. A trigger enforces the same rule in the database.
///
/// # Errors
/// - [`RepoError::NotFound`] when the user has no program with that id.
/// - [`RepoError::ProgramArchived`] when the program is archived.
pub async fn set(pool: &PgPool, user: UserId, program: ProgramId) -> Result<(), RepoError> {
    let mut tx = pool.begin().await?;
    let archived = sqlx::query_scalar!(
        "SELECT archived FROM programs WHERE id = $1 AND user_id = $2 FOR UPDATE",
        program.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(&mut *tx)
    .await?
    // Not the user's (someone else's, a built-in) or no such program.
    .ok_or(RepoError::NotFound)?;
    if archived {
        return Err(RepoError::ProgramArchived);
    }
    sqlx::query!(
        "INSERT INTO active_program (user_id, program_id) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET program_id = EXCLUDED.program_id, updated_at = now()",
        user.as_uuid(),
        program.as_uuid(),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Clears the user's active program. Clearing when none is set is a no-op.
pub async fn clear(pool: &PgPool, user: UserId) -> Result<(), RepoError> {
    sqlx::query!(
        "DELETE FROM active_program WHERE user_id = $1",
        user.as_uuid()
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{
        MIGRATOR, programs,
        testing::{self, random_uuid},
    };

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn set_replace_and_clear(pool: PgPool) {
        let user = testing::user(&pool).await;
        assert_eq!(get(&pool, user).await.unwrap(), None);
        let (first, _) = testing::program(&pool, user).await;
        let (second, _) = testing::program(&pool, user).await;
        set(&pool, user, first).await.unwrap();
        set(&pool, user, first).await.unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), Some(first));
        set(&pool, user, second).await.unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), Some(second));
        clear(&pool, user).await.unwrap();
        clear(&pool, user).await.unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), None);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn only_the_users_own_programs_can_be_made_active(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let (program_a, _) = testing::program(&pool, a).await;
        set(&pool, a, program_a).await.unwrap();
        programs::seed_builtins(
            &pool,
            &[programs::BuiltinSeed {
                builtin_id: "starter",
                name: "Starter",
                json: r#"{"schema_version": 1}"#,
            }],
        )
        .await
        .unwrap();
        let builtin = programs::list_builtins(&pool).await.unwrap()[0].program.id;

        for program in [program_a, builtin, ProgramId::from_uuid(random_uuid())] {
            let result = set(&pool, b, program).await;
            assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
        }
        // B sees no active program and clearing B's does not touch A's.
        assert_eq!(get(&pool, b).await.unwrap(), None);
        clear(&pool, b).await.unwrap();
        assert_eq!(get(&pool, a).await.unwrap(), Some(program_a));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn an_archived_program_cannot_be_made_active(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (program, _) = testing::program(&pool, user).await;
        programs::set_archived(&pool, user, program, true)
            .await
            .unwrap();
        let result = set(&pool, user, program).await;
        assert!(
            matches!(result, Err(RepoError::ProgramArchived)),
            "{result:?}"
        );
        assert_eq!(get(&pool, user).await.unwrap(), None);
        // Restored, it can.
        programs::set_archived(&pool, user, program, false)
            .await
            .unwrap();
        set(&pool, user, program).await.unwrap();
        let archive = programs::set_archived(&pool, user, program, true).await;
        assert!(
            matches!(archive, Err(RepoError::ProgramActive)),
            "{archive:?}"
        );
    }

    /// The triggers keep the rule for writes that skip the repository's checks.
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn the_database_refuses_an_archived_active_program(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (active, _) = testing::program(&pool, user).await;
        let (archived, _) = testing::program(&pool, user).await;
        set(&pool, user, active).await.unwrap();
        programs::set_archived(&pool, user, archived, true)
            .await
            .unwrap();

        let archive_active = sqlx::query("UPDATE programs SET archived = true WHERE id = $1")
            .bind(active.as_uuid())
            .execute(&pool)
            .await
            .map_err(RepoError::from);
        assert!(
            matches!(archive_active, Err(RepoError::ProgramActive)),
            "{archive_active:?}"
        );
        let point_at_archived =
            sqlx::query("UPDATE active_program SET program_id = $2 WHERE user_id = $1")
                .bind(user.as_uuid())
                .bind(archived.as_uuid())
                .execute(&pool)
                .await
                .map_err(RepoError::from);
        assert!(
            matches!(point_at_archived, Err(RepoError::ProgramArchived)),
            "{point_at_archived:?}"
        );
        clear(&pool, user).await.unwrap();
        let insert_archived =
            sqlx::query("INSERT INTO active_program (user_id, program_id) VALUES ($1, $2)")
                .bind(user.as_uuid())
                .bind(archived.as_uuid())
                .execute(&pool)
                .await
                .map_err(RepoError::from);
        assert!(
            matches!(insert_archived, Err(RepoError::ProgramArchived)),
            "{insert_archived:?}"
        );
        // Archiving an archived program again, or a program nobody trains with, is fine.
        sqlx::query("UPDATE programs SET archived = true WHERE id IN ($1, $2)")
            .bind(active.as_uuid())
            .bind(archived.as_uuid())
            .execute(&pool)
            .await
            .unwrap();
    }

    /// B pointing at A's archived program learns nothing about it: not found, as for any id.
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn another_users_archived_program_is_not_found(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let (program, _) = testing::program(&pool, a).await;
        programs::set_archived(&pool, a, program, true)
            .await
            .unwrap();
        let result = set(&pool, b, program).await;
        assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
        let raw = sqlx::query("INSERT INTO active_program (user_id, program_id) VALUES ($1, $2)")
            .bind(b.as_uuid())
            .bind(program.as_uuid())
            .execute(&pool)
            .await
            .map_err(RepoError::from);
        assert!(matches!(raw, Err(RepoError::NotFound)), "{raw:?}");
    }
}
