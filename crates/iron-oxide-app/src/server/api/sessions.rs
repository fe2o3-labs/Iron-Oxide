//! Sessions (#18): the logic behind `crate::api::sessions`.

use iron_oxide_domain::{DayId, SessionId, SessionStatus};
use sqlx::PgPool;

use super::{ApiError, timestamp};
use crate::api::sessions::SessionView;
use crate::server::db::{self, ids::UserId};

/// `owner`'s session `id`.
pub async fn get(pool: &PgPool, owner: UserId, id: SessionId) -> Result<SessionView, ApiError> {
    view(db::sessions::get(pool, owner, id.into()).await?)
}

/// A stored session as the client sees it.
pub fn view(session: db::sessions::WorkoutSession) -> Result<SessionView, ApiError> {
    Ok(SessionView {
        id: session.id.into(),
        program_id: session.program_id.into(),
        program_version_id: session.program_version_id.into(),
        day: DayId::new(session.day_id).map_err(ApiError::internal)?,
        status: status(session.status),
        started_at: timestamp(session.started_at)?,
        finished_at: session.finished_at.map(timestamp).transpose()?,
    })
}

pub(super) const fn status(status: db::sessions::SessionStatus) -> SessionStatus {
    match status {
        db::sessions::SessionStatus::InProgress => SessionStatus::InProgress,
        db::sessions::SessionStatus::Completed => SessionStatus::Completed,
        db::sessions::SessionStatus::Skipped => SessionStatus::Skipped,
        db::sessions::SessionStatus::Abandoned => SessionStatus::Abandoned,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::{PgPool, types::Uuid};

    use super::*;
    use crate::server::{
        api::testing::{self, TestApi},
        db::{
            ids,
            sessions::{SessionOutcome, WorkoutSession},
            testing as db_testing,
        },
    };

    const GET: &str = "/api/sessions/get";

    fn stored(day: &str, status: db::sessions::SessionStatus) -> WorkoutSession {
        WorkoutSession {
            id: ids::SessionId::from_uuid(Uuid::from_u128(1)),
            program_version_id: ids::ProgramVersionId::from_uuid(Uuid::from_u128(2)),
            program_id: ids::ProgramId::from_uuid(Uuid::from_u128(3)),
            day_id: day.to_owned(),
            status,
            started_at: db_testing::at(0),
            finished_at: None,
        }
    }

    #[test]
    fn view_converts_every_status() {
        for (stored_status, expected) in [
            (
                db::sessions::SessionStatus::InProgress,
                SessionStatus::InProgress,
            ),
            (
                db::sessions::SessionStatus::Completed,
                SessionStatus::Completed,
            ),
            (db::sessions::SessionStatus::Skipped, SessionStatus::Skipped),
            (
                db::sessions::SessionStatus::Abandoned,
                SessionStatus::Abandoned,
            ),
        ] {
            assert_eq!(view(stored("a", stored_status)).unwrap().status, expected);
        }
    }

    #[test]
    fn a_stored_day_that_is_not_a_slug_is_an_internal_error() {
        let error = view(stored("Day A", db::sessions::SessionStatus::InProgress)).unwrap_err();
        assert_eq!(error.public().0, 500);
    }

    #[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn get_session_returns_the_users_own_session(db: PgPool) {
        let api = TestApi::new(db).await;
        let mut a = api.user("A").await;
        let session = db_testing::session(&api.db, a.id).await;
        db::sessions::finish(
            &api.db,
            a.id,
            session,
            SessionOutcome::Completed,
            db_testing::at(90),
        )
        .await
        .unwrap();

        let view: SessionView = a
            .call(GET, json!({ "session_id": session.as_uuid() }))
            .await
            .unwrap();
        assert_eq!(view.id.as_uuid(), session.as_uuid());
        assert_eq!(view.day.as_str(), "a");
        assert_eq!(view.status, SessionStatus::Completed);
        assert_eq!(view.started_at.epoch_millis(), 1_790_000_000_000);
        assert_eq!(
            view.finished_at.map(|t| t.epoch_millis()),
            Some(1_790_000_090_000)
        );
    }

    #[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn another_users_session_is_not_found(db: PgPool) {
        let api = TestApi::new(db).await;
        let (mut a, mut b) = api.users_a_and_b().await;
        let session = db_testing::session(&api.db, a.id).await;
        let body = |id: Uuid| json!({ "session_id": id });

        testing::assert_not_found_for_other_user(&mut b, GET, session.as_uuid(), body).await;
        // A still sees it: the 404 was about B, not about the id.
        let view: Result<SessionView, _> = a.call(GET, body(session.as_uuid())).await;
        assert!(view.is_ok(), "{view:?}");
    }

    #[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn arguments_that_do_not_decode_are_422(db: PgPool) {
        let api = TestApi::new(db).await;
        let mut a = api.user("A").await;
        for body in [
            json!({ "session_id": "not-a-uuid" }),
            json!({ "session_id": 5 }),
            json!({}),
        ] {
            let error = a.call_err(GET, body).await;
            assert_eq!(
                error,
                testing::CallError {
                    status: dioxus::server::axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    message: "Invalid request.".to_owned(),
                }
            );
        }
    }

    #[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn get_session_needs_a_signed_in_user(db: PgPool) {
        let api = TestApi::new(db).await;
        let a = api.user("A").await;
        let session = db_testing::session(&api.db, a.id).await;
        testing::assert_unauthorized_when_signed_out(
            &api,
            GET,
            json!({ "session_id": session.as_uuid() }),
        )
        .await;
    }
}
