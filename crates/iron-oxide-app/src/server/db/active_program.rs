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
