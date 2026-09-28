//! Plan gating on the server (#21).
//!
//! The policy itself lives in `iron_oxide_domain::entitlements` (`allows`, `limit`, `can_add`);
//! this module reads the signed-in user's plan from `users.plan` and turns a refusal into a
//! `403`. Server functions gate a feature with [`require`], and every write that takes a quota
//! slot with [`reserve_quota`], inside the transaction that writes. Nothing else looks at a plan.
//!
//! The plan is read on every call and never cached, so flipping `users.plan` (billing, or an
//! operator in SQL) changes what the user may do on their next request.
//!
//! ```rust,ignore
//! #[post("/api/programs/upload", state: Extension<AppState>, user: AuthUser)]
//! pub async fn upload_program(/* ... */) -> Result<(), ServerFnError> {
//!     entitlements::require(&state.db, user, Feature::UploadPrograms).await?;
//!     // ...
//! }
//! ```

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;
use iron_oxide_domain::entitlements::{self as policy, Entitlements, Feature, Limit, Plan, Quota};
use sqlx::{PgConnection, PgPool};

use super::{
    auth::AuthUser,
    db::{self, error::RepoError},
};

/// Why a gated call was refused, or could not be checked.
#[derive(Debug, thiserror::Error)]
pub enum EntitlementError {
    /// The user's plan does not include the feature.
    #[error("{feature:?} is not included in the {plan} plan")]
    FeatureNotIncluded { feature: Feature, plan: Plan },
    /// The user's plan caps the quota and it is used up.
    #[error("{quota:?} quota reached on the {plan} plan ({used} used, limit {limit:?})")]
    QuotaReached {
        quota: Quota,
        plan: Plan,
        used: u32,
        limit: Limit,
    },
    /// The session's user no longer exists (deleted meanwhile).
    #[error("the signed-in user no longer exists")]
    UnknownUser,
    /// Reading the plan failed.
    #[error("cannot read the plan: {0}")]
    Repo(#[from] RepoError),
}

impl EntitlementError {
    /// The HTTP status and the message shown to the user.
    ///
    // TODO(#68): map to `ApiError::Forbidden(message)` (403) / `Unauthorized` / `Transient` /
    // `Internal` once #68's API conventions (PR #71) land, and drop this local mapping.
    #[must_use]
    pub fn public(&self) -> (u16, String) {
        match self {
            Self::FeatureNotIncluded { .. } => {
                (403, "This feature is part of Iron Oxide Pro.".to_owned())
            }
            Self::QuotaReached {
                quota: Quota::CustomPrograms,
                limit,
                ..
            } => {
                let message = match limit {
                    Limit::AtMost { max } => format!(
                        "Your plan keeps up to {max} programs. Archive one, or upgrade to Pro."
                    ),
                    Limit::Unlimited => "Your plan's program limit is reached.".to_owned(),
                };
                (403, message)
            }
            Self::UnknownUser => (401, "Please sign in.".to_owned()),
            Self::Repo(RepoError::Transient) => (503, "Please try again.".to_owned()),
            Self::Repo(_) => (500, "Something went wrong. Please try again.".to_owned()),
        }
    }

    fn log(&self) {
        match self {
            Self::Repo(RepoError::Transient) => tracing::warn!(error = %self, "plan check failed"),
            Self::Repo(_) => tracing::error!(error = %self, "plan check failed"),
            Self::UnknownUser => tracing::debug!(error = %self, "plan check without a user"),
            Self::FeatureNotIncluded { .. } | Self::QuotaReached { .. } => {
                tracing::info!(error = %self, "refused by the plan");
            }
        }
    }
}

impl From<EntitlementError> for ServerFnError {
    fn from(error: EntitlementError) -> Self {
        error.log();
        let (code, message) = error.public();
        ServerFnError::ServerError {
            message,
            code,
            details: None,
        }
    }
}

/// The signed-in user's plan, read from `users.plan`.
pub async fn plan_of(db: &PgPool, user: AuthUser) -> Result<Plan, EntitlementError> {
    let id = db::ids::UserId::from_uuid(user.user_id().as_uuid());
    db::users::plan(db, id)
        .await?
        .ok_or(EntitlementError::UnknownUser)
}

/// Everything the signed-in user's plan includes.
pub async fn entitlements_of(
    db: &PgPool,
    user: AuthUser,
) -> Result<Entitlements, EntitlementError> {
    Ok(Entitlements::of(plan_of(db, user).await?))
}

/// Succeeds when `plan` includes `feature`.
pub fn check(plan: Plan, feature: Feature) -> Result<(), EntitlementError> {
    if policy::allows(plan, feature) {
        Ok(())
    } else {
        Err(EntitlementError::FeatureNotIncluded { feature, plan })
    }
}

/// Succeeds when a user on `plan` who already has `used` of `quota` may add one more.
///
/// The pure check. A write that takes a slot uses [`reserve_quota`], which counts `used` under the
/// user's row lock.
pub fn check_quota(plan: Plan, quota: Quota, used: u32) -> Result<(), EntitlementError> {
    if policy::can_add(plan, quota, used) {
        Ok(())
    } else {
        Err(EntitlementError::QuotaReached {
            quota,
            plan,
            used,
            limit: policy::limit(plan, quota),
        })
    }
}

/// Fails with `403` unless the signed-in user's plan includes `feature`. Returns the plan.
#[allow(
    dead_code,
    reason = "called by the gated server functions of #19 and #20"
)]
pub async fn require(
    db: &PgPool,
    user: AuthUser,
    feature: Feature,
) -> Result<Plan, EntitlementError> {
    let plan = plan_of(db, user).await?;
    check(plan, feature)?;
    Ok(plan)
}

/// Takes one slot of `quota` for the signed-in user, or fails with `403` when their plan's limit
/// is reached. Returns the plan.
///
/// Call it inside the transaction that then adds the row, **before** writing: it locks the user's
/// row (`SELECT … FROM users … FOR UPDATE`, held until the transaction ends), reads the plan, and
/// counts what the quota counts, all under that lock. Two concurrent requests for the same user
/// are therefore serialised: the second one counts the first one's row and is refused at the cap.
/// Counting on a pool outside such a transaction is always racy, so there is no pool variant.
///
/// Every write that raises a quota's count must go through it. For
/// [`Quota::CustomPrograms`] (unarchived programs the user owns): creating or uploading a new
/// program, copying a built-in, and **unarchiving** a program. Archiving, renaming, adding a
/// version to an existing program and an idempotent replay (the same creation id, answered from
/// the existing row) take no slot: check for the replay first, so a retry is never refused.
///
/// A user over the cap (a downgrade from pro) keeps every program and can use and archive them,
/// but cannot add or unarchive one until they are under the cap.
#[allow(dead_code, reason = "called by the program server functions of #19")]
pub async fn reserve_quota(
    tx: &mut PgConnection,
    user: AuthUser,
    quota: Quota,
) -> Result<Plan, EntitlementError> {
    let id = db::ids::UserId::from_uuid(user.user_id().as_uuid());
    let plan = db::users::lock_plan(&mut *tx, id)
        .await?
        .ok_or(EntitlementError::UnknownUser)?;
    let used = match quota {
        Quota::CustomPrograms => db::users::unarchived_programs(&mut *tx, id).await?,
    };
    check_quota(plan, quota, used)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::Me;
    use crate::auth::types::UserId;
    use crate::server::auth::test_support::{Browser, CallError, Passkey, TestApp};
    use crate::server::db::{MIGRATOR, testing};
    use dioxus::server::axum::http::StatusCode;
    use iron_oxide_domain::entitlements::FREE_CUSTOM_PROGRAMS;
    use serde_json::json;
    use webauthn_rs_proto::CreationChallengeResponse;

    const ENTITLEMENTS: &str = "/api/billing/entitlements";

    fn code(error: EntitlementError) -> (u16, String) {
        let ServerFnError::ServerError { code, message, .. } = ServerFnError::from(error) else {
            panic!("not a server error");
        };
        (code, message)
    }

    fn auth_user(id: db::ids::UserId) -> AuthUser {
        AuthUser::for_tests(UserId::from_uuid(id.as_uuid()))
    }

    async fn set_plan(pool: &PgPool, user: db::ids::UserId, plan: Plan) {
        sqlx::query("UPDATE users SET plan = $2::user_plan WHERE id = $1")
            .bind(user.as_uuid())
            .bind(plan.as_str())
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn every_feature_passes_the_check_on_every_plan_for_now() {
        for plan in Plan::ALL {
            for feature in Feature::ALL {
                assert!(check(plan, feature).is_ok(), "{plan:?} {feature:?}");
            }
        }
    }

    #[test]
    fn a_feature_refusal_is_403() {
        let error = EntitlementError::FeatureNotIncluded {
            feature: Feature::ExerciseCharts,
            plan: Plan::Free,
        };
        assert_eq!(
            code(error),
            (403, "This feature is part of Iron Oxide Pro.".to_owned())
        );
    }

    #[test]
    fn the_custom_program_quota_is_403_at_the_free_limit_only() {
        let q = Quota::CustomPrograms;
        assert!(check_quota(Plan::Free, q, FREE_CUSTOM_PROGRAMS - 1).is_ok());
        let error = check_quota(Plan::Free, q, FREE_CUSTOM_PROGRAMS).unwrap_err();
        assert!(matches!(
            error,
            EntitlementError::QuotaReached {
                plan: Plan::Free,
                used: FREE_CUSTOM_PROGRAMS,
                limit: Limit::AtMost {
                    max: FREE_CUSTOM_PROGRAMS
                },
                ..
            }
        ));
        let (status, message) = code(error);
        assert_eq!(status, 403);
        assert!(message.contains("up to 10 programs"), "{message}");
        assert!(check_quota(Plan::Pro, q, FREE_CUSTOM_PROGRAMS).is_ok());
        assert!(check_quota(Plan::Pro, q, u32::MAX).is_ok());
    }

    #[test]
    fn other_failures_keep_their_details_private() {
        assert_eq!(code(EntitlementError::UnknownUser).0, 401);
        assert_eq!(code(EntitlementError::Repo(RepoError::Transient)).0, 503);
        let (status, message) = code(EntitlementError::Repo(RepoError::Database(
            sqlx::Error::Protocol("secret at 10.0.0.3".to_owned()),
        )));
        assert_eq!(status, 500);
        assert!(!message.contains("10.0.0.3"), "{message}");
        let (status, message) = code(EntitlementError::Repo(RepoError::Corrupt("users.plan")));
        assert_eq!(status, 500);
        assert!(!message.contains("users.plan"), "{message}");
        // An unlimited quota never refuses, but the message stays sensible if it ever did.
        let (status, _) = code(EntitlementError::QuotaReached {
            quota: Quota::CustomPrograms,
            plan: Plan::Pro,
            used: 0,
            limit: Limit::Unlimited,
        });
        assert_eq!(status, 403);
    }

    /// The ticket's acceptance: flipping `users.plan` changes the entitlements, with no other
    /// change (no cache, no session state).
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn flipping_the_plan_in_the_database_changes_the_entitlements(pool: PgPool) {
        let id = testing::user(&pool).await;
        let user = auth_user(id);
        let q = Quota::CustomPrograms;
        add_programs(&pool, id, FREE_CUSTOM_PROGRAMS, false).await;

        assert_eq!(
            entitlements_of(&pool, user).await.unwrap(),
            Entitlements::of(Plan::Free)
        );
        let (status, _) = code(reserve(&pool, user, q).await.unwrap_err());
        assert_eq!(status, 403);

        set_plan(&pool, id, Plan::Pro).await;
        assert_eq!(
            entitlements_of(&pool, user).await.unwrap(),
            Entitlements::of(Plan::Pro)
        );
        assert_eq!(reserve(&pool, user, q).await.unwrap(), Plan::Pro);

        set_plan(&pool, id, Plan::Free).await;
        assert_eq!(plan_of(&pool, user).await.unwrap(), Plan::Free);
        assert!(reserve(&pool, user, q).await.is_err());
    }

    /// Adds `n` programs owned by `user`, as the program server functions will (#19).
    async fn add_programs(pool: &PgPool, user: db::ids::UserId, n: u32, archived: bool) {
        for _ in 0..n {
            insert_program(&mut pool.acquire().await.unwrap(), user, archived).await;
        }
    }

    async fn insert_program(conn: &mut PgConnection, user: db::ids::UserId, archived: bool) {
        sqlx::query(
            "INSERT INTO programs (user_id, creation_id, name, archived)
             VALUES ($1, $2, 'Program', $3)",
        )
        .bind(user.as_uuid())
        .bind(testing::random_uuid())
        .bind(archived)
        .execute(conn)
        .await
        .unwrap();
    }

    /// [`reserve_quota`] in a transaction of its own, rolled back.
    async fn reserve(
        pool: &PgPool,
        user: AuthUser,
        quota: Quota,
    ) -> Result<Plan, EntitlementError> {
        let mut tx = pool.begin().await.unwrap();
        let result = reserve_quota(&mut tx, user, quota).await;
        tx.rollback().await.unwrap();
        result
    }

    /// The recipe for every quota write (#19): reserve, then insert, in one transaction. Two
    /// concurrent "create" requests of a free user at 9 programs end at 10, never 11.
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn two_concurrent_reservations_cannot_both_take_the_last_slot(pool: PgPool) {
        let id = testing::user(&pool).await;
        let user = auth_user(id);
        add_programs(&pool, id, FREE_CUSTOM_PROGRAMS - 1, false).await;

        // A reserves the last slot and holds the user's row lock.
        let mut a = pool.begin().await.unwrap();
        reserve_quota(&mut a, user, Quota::CustomPrograms)
            .await
            .unwrap();

        // B, concurrently, must wait for A's lock rather than count 9 too.
        let b = tokio::spawn({
            let pool = pool.clone();
            async move {
                let mut b = pool.begin().await.unwrap();
                let result = reserve_quota(&mut b, user, Quota::CustomPrograms).await;
                if result.is_ok() {
                    insert_program(&mut b, id, false).await;
                }
                b.commit().await.unwrap();
                result
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!b.is_finished(), "B must block on A's row lock");

        insert_program(&mut a, id, false).await;
        a.commit().await.unwrap();

        let error = b.await.unwrap().unwrap_err();
        assert!(
            matches!(error, EntitlementError::QuotaReached { used: 10, .. }),
            "{error:?}"
        );
        let count = db::users::unarchived_programs(&pool, id).await.unwrap();
        assert_eq!(count, FREE_CUSTOM_PROGRAMS);
    }

    /// Unarchiving takes a slot; archived programs do not count; a downgraded user over the cap
    /// keeps everything but cannot add or unarchive until under the cap.
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn only_unarchived_programs_count_and_a_downgrade_keeps_everything(pool: PgPool) {
        let id = testing::user(&pool).await;
        let user = auth_user(id);
        let q = Quota::CustomPrograms;
        add_programs(&pool, id, 5, true).await;
        add_programs(&pool, id, FREE_CUSTOM_PROGRAMS - 1, false).await;
        assert_eq!(
            db::users::unarchived_programs(&pool, id).await.unwrap(),
            FREE_CUSTOM_PROGRAMS - 1
        );
        // One slot left: unarchiving one is allowed, a second is not.
        assert!(reserve(&pool, user, q).await.is_ok());
        sqlx::query(
            "UPDATE programs SET archived = false
             WHERE id = (SELECT id FROM programs WHERE user_id = $1 AND archived LIMIT 1)",
        )
        .bind(id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
        assert!(reserve(&pool, user, q).await.is_err());

        // Pro goes over the cap, then is downgraded: nothing is removed, nothing more is allowed.
        set_plan(&pool, id, Plan::Pro).await;
        add_programs(&pool, id, 3, false).await;
        set_plan(&pool, id, Plan::Free).await;
        assert_eq!(
            db::users::unarchived_programs(&pool, id).await.unwrap(),
            FREE_CUSTOM_PROGRAMS + 3
        );
        assert!(reserve(&pool, user, q).await.is_err());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn nobody_can_reserve_without_an_account(pool: PgPool) {
        let ghost = auth_user(db::ids::UserId::from_uuid(testing::random_uuid()));
        let error = reserve(&pool, ghost, Quota::CustomPrograms)
            .await
            .unwrap_err();
        assert!(matches!(error, EntitlementError::UnknownUser));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn require_reads_the_plan_and_checks_the_feature(pool: PgPool) {
        let id = testing::user(&pool).await;
        let user = auth_user(id);
        for feature in Feature::ALL {
            assert_eq!(require(&pool, user, feature).await.unwrap(), Plan::Free);
        }
        set_plan(&pool, id, Plan::Pro).await;
        assert_eq!(
            require(&pool, user, Feature::UploadPrograms).await.unwrap(),
            Plan::Pro
        );
    }

    /// Signs up a new account with a passkey in `browser`; returns its id.
    async fn sign_up(browser: &mut Browser) -> UserId {
        let mut passkey = Passkey::new();
        let ccr: CreationChallengeResponse = browser
            .call(
                "/api/auth/passkey/sign-up/begin",
                json!({ "display_name": "" }),
            )
            .await
            .unwrap();
        let credential = passkey.register(ccr);
        let me: Me = browser
            .call(
                "/api/auth/passkey/sign-up/finish",
                json!({ "credential": credential }),
            )
            .await
            .unwrap();
        me.user_id
    }

    async fn my_entitlements(browser: &mut Browser) -> Result<Entitlements, CallError> {
        browser.call(ENTITLEMENTS, json!({})).await
    }

    /// The acceptance, end to end: the server function, signed in, before and after the flip.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_entitlements_endpoint_follows_the_database_plan(pool: PgPool) {
        let app = TestApp::new(pool.clone()).await;
        let mut browser = app.browser();
        let id = sign_up(&mut browser).await;
        let id = db::ids::UserId::from_uuid(id.as_uuid());

        let before = my_entitlements(&mut browser).await.unwrap();
        assert_eq!(before, Entitlements::of(Plan::Free));
        assert_eq!(
            before.limit(Quota::CustomPrograms),
            Some(Limit::AtMost {
                max: FREE_CUSTOM_PROGRAMS
            })
        );

        set_plan(&pool, id, Plan::Pro).await;
        let after = my_entitlements(&mut browser).await.unwrap();
        assert_eq!(after, Entitlements::of(Plan::Pro));
        assert_eq!(after.limit(Quota::CustomPrograms), Some(Limit::Unlimited));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_users_each_get_their_own_plan(pool: PgPool) {
        let app = TestApp::new(pool.clone()).await;
        let mut a = app.browser();
        let mut b = app.browser();
        let a_id = sign_up(&mut a).await;
        sign_up(&mut b).await;
        set_plan(&pool, db::ids::UserId::from_uuid(a_id.as_uuid()), Plan::Pro).await;
        assert_eq!(my_entitlements(&mut a).await.unwrap().plan, Plan::Pro);
        assert_eq!(my_entitlements(&mut b).await.unwrap().plan, Plan::Free);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_entitlements_endpoint_needs_a_session_and_a_same_origin_post(pool: PgPool) {
        let app = TestApp::new(pool).await;
        let mut signed_out = app.browser();
        let error = my_entitlements(&mut signed_out).await.unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);

        let mut cross_site = app.browser();
        sign_up(&mut cross_site).await;
        cross_site.origin = Some("https://evil.example".to_owned());
        cross_site.fetch_site = Some("cross-site".to_owned());
        let error = my_entitlements(&mut cross_site).await.unwrap_err();
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn nobody_can_be_gated_without_an_account(pool: PgPool) {
        let ghost = auth_user(db::ids::UserId::from_uuid(testing::random_uuid()));
        let error = require(&pool, ghost, Feature::UploadPrograms)
            .await
            .unwrap_err();
        assert!(matches!(error, EntitlementError::UnknownUser));
        assert_eq!(code(error).0, 401);
    }
}
