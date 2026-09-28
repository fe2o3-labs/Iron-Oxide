//! Accounts (`users`): the subscription plan.
//!
//! The plan is read from the database on every call, never cached in the session: a plan that
//! billing (or an operator) flips in `users.plan` applies to the user's very next request.

use iron_oxide_domain::entitlements::Plan;
use sqlx::{PgConnection, PgExecutor};

use super::{
    error::{RepoError, narrow},
    ids::UserId,
};

/// The user's plan, or `None` if the user does not exist (deleted).
///
/// Takes any executor, so a caller that checks a quota and then writes can read the plan inside
/// its own transaction.
pub async fn plan<'e>(
    executor: impl PgExecutor<'e>,
    user: UserId,
) -> Result<Option<Plan>, RepoError> {
    let plan = sqlx::query_scalar!(
        r#"SELECT plan::text AS "plan!" FROM users WHERE id = $1"#,
        user.as_uuid()
    )
    .fetch_optional(executor)
    .await?;
    plan.map(|name| parse_plan(&name)).transpose()
}

/// Locks the user's row until the end of the transaction (`SELECT … FOR UPDATE`) and returns the
/// plan, or `None` if the user does not exist.
///
/// This is the serialisation point of every quota check: two transactions that both lock the
/// same user run one after the other, so the second one counts what the first one added.
pub async fn lock_plan(tx: &mut PgConnection, user: UserId) -> Result<Option<Plan>, RepoError> {
    let plan = sqlx::query_scalar!(
        r#"SELECT plan::text AS "plan!" FROM users WHERE id = $1 FOR UPDATE"#,
        user.as_uuid()
    )
    .fetch_optional(tx)
    .await?;
    plan.map(|name| parse_plan(&name)).transpose()
}

/// How many programs the user owns and has not archived: what `Quota::CustomPrograms` counts
/// (uploaded programs and copies of built-ins alike).
pub async fn unarchived_programs<'e>(
    executor: impl PgExecutor<'e>,
    user: UserId,
) -> Result<u32, RepoError> {
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM programs WHERE user_id = $1 AND NOT archived"#,
        user.as_uuid()
    )
    .fetch_one(executor)
    .await?;
    narrow(count, "programs (unarchived count)")
}

/// The stored enum value as a [`Plan`]. The `user_plan` enum and [`Plan`] must list the same
/// values; anything else means the schema and the code disagree.
fn parse_plan(name: &str) -> Result<Plan, RepoError> {
    name.parse().map_err(|_| RepoError::Corrupt("users.plan"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{MIGRATOR, testing};
    use sqlx::PgPool;

    async fn set_plan(pool: &PgPool, user: UserId, plan: &str) {
        sqlx::query("UPDATE users SET plan = $2::user_plan WHERE id = $1")
            .bind(user.as_uuid())
            .bind(plan)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn stored_names_parse_and_unknown_ones_are_corrupt() {
        assert_eq!(parse_plan("free").unwrap(), Plan::Free);
        assert_eq!(parse_plan("pro").unwrap(), Plan::Pro);
        assert!(matches!(
            parse_plan("team"),
            Err(RepoError::Corrupt("users.plan"))
        ));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_new_user_is_on_the_free_plan(pool: PgPool) {
        let user = testing::user(&pool).await;
        assert_eq!(plan(&pool, user).await.unwrap(), Some(Plan::Free));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn every_database_plan_value_parses(pool: PgPool) {
        // Every value of the `user_plan` enum maps to a `Plan`, and back.
        let names: Vec<String> =
            sqlx::query_scalar("SELECT unnest(enum_range(NULL::user_plan))::text")
                .fetch_all(&pool)
                .await
                .unwrap();
        let parsed: Vec<Plan> = names.iter().map(|n| parse_plan(n).unwrap()).collect();
        assert_eq!(parsed, Plan::ALL);
        let user = testing::user(&pool).await;
        for p in Plan::ALL {
            set_plan(&pool, user, p.as_str()).await;
            assert_eq!(plan(&pool, user).await.unwrap(), Some(p));
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_missing_user_has_no_plan(pool: PgPool) {
        let nobody = UserId::from_uuid(testing::random_uuid());
        assert_eq!(plan(&pool, nobody).await.unwrap(), None);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn only_the_users_own_plan_is_read(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        set_plan(&pool, a, "pro").await;
        assert_eq!(plan(&pool, a).await.unwrap(), Some(Plan::Pro));
        assert_eq!(plan(&pool, b).await.unwrap(), Some(Plan::Free));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn the_plan_can_be_read_inside_a_transaction(pool: PgPool) {
        let user = testing::user(&pool).await;
        let mut tx = pool.begin().await.unwrap();
        set_plan_in(&mut tx, user).await;
        assert_eq!(plan(&mut *tx, user).await.unwrap(), Some(Plan::Pro));
        tx.rollback().await.unwrap();
        assert_eq!(plan(&pool, user).await.unwrap(), Some(Plan::Free));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn lock_plan_reads_the_plan_and_a_missing_user_has_none(pool: PgPool) {
        let user = testing::user(&pool).await;
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(lock_plan(&mut tx, user).await.unwrap(), Some(Plan::Free));
        let nobody = UserId::from_uuid(testing::random_uuid());
        assert_eq!(lock_plan(&mut tx, nobody).await.unwrap(), None);
        tx.rollback().await.unwrap();
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn unarchived_programs_counts_only_the_users_own_unarchived_ones(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let (archived, _) = testing::program(&pool, a).await;
        super::super::programs::set_archived(&pool, a, archived, true)
            .await
            .unwrap();
        testing::program(&pool, a).await;
        testing::program(&pool, a).await;
        testing::program(&pool, b).await;
        assert_eq!(unarchived_programs(&pool, a).await.unwrap(), 2);
        assert_eq!(unarchived_programs(&pool, b).await.unwrap(), 1);
        let nobody = UserId::from_uuid(testing::random_uuid());
        assert_eq!(unarchived_programs(&pool, nobody).await.unwrap(), 0);
    }

    async fn set_plan_in(tx: &mut sqlx::PgConnection, user: UserId) {
        sqlx::query("UPDATE users SET plan = 'pro' WHERE id = $1")
            .bind(user.as_uuid())
            .execute(tx)
            .await
            .unwrap();
    }
}
