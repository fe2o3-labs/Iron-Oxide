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
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id.
pub async fn set(pool: &PgPool, user: UserId, program: ProgramId) -> Result<(), RepoError> {
    // The composite foreign key turns someone else's (or a built-in) program id into a foreign key
    // violation, which maps to NotFound like a program that does not exist.
    sqlx::query!(
        "INSERT INTO active_program (user_id, program_id) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET program_id = EXCLUDED.program_id, updated_at = now()",
        user.as_uuid(),
        program.as_uuid(),
    )
    .execute(pool)
    .await?;
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
}
