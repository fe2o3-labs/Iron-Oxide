//! Workout sessions (#18): starting, reading and finishing sessions.

use dioxus::prelude::*;
use iron_oxide_domain::{
    DayId, ProgramId, ProgramVersionId, SessionId, SessionStatus, time::Timestamp,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use {
    crate::server::{AppState, api::sessions, auth::AuthUser},
    dioxus::server::axum::Extension,
};

/// A workout session, as the client sees it.
#[cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the client only decodes it until the session screens (#29) land"
    )
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: SessionId,
    pub program_id: ProgramId,
    pub program_version_id: ProgramVersionId,
    pub day: DayId,
    pub status: SessionStatus,
    pub started_at: Timestamp,
    /// `None` while in progress.
    pub finished_at: Option<Timestamp>,
}

/// One of the signed-in user's sessions. `404` when it does not exist or is someone else's.
#[post("/api/sessions/get", state: Extension<AppState>, user: AuthUser)]
pub async fn get_session(session_id: SessionId) -> Result<SessionView, ServerFnError> {
    Ok(sessions::get(&state.db, user.owner(), session_id).await?)
}
