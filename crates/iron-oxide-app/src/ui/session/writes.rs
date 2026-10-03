//! Every write of a workout session goes through this file: one function per write, with the
//! exact arguments of its server function (and of the offline outbox's `Write::StartSession`,
//! `Write::SaveSet` and `Write::FinishSession`, #30). Today each one calls the server; switching
//! them to the outbox is a change to this file only.
//!
//! The ids are client-generated UUIDv7 (`SessionId::new_v7()`, `SetId::new_v7()`) and the
//! timestamps come from the client, so the same call sent again is idempotent: callers keep the
//! arguments of a failed write and resend them unchanged.

use dioxus::prelude::*;
use iron_oxide_domain::time::Timestamp;
use iron_oxide_domain::{LoggedSet, SessionId, SessionOutcome};

use crate::api::sessions::{self, SessionSummary, SessionView};

/// Starts a session of the active program on the next day of its rotation.
pub async fn start_session(
    session_id: SessionId,
    started_at: Timestamp,
) -> Result<SessionView, ServerFnError> {
    sessions::start_session(session_id, started_at).await
}

/// Saves one logged set.
pub async fn save_set(
    session_id: SessionId,
    set: LoggedSet<Timestamp>,
) -> Result<(), ServerFnError> {
    sessions::save_set(session_id, set).await
}

/// Ends the session and returns its summary. Sending it again returns the same summary.
pub async fn finish_session(
    session_id: SessionId,
    outcome: SessionOutcome,
    finished_at: Timestamp,
) -> Result<SessionSummary, ServerFnError> {
    sessions::finish_session(session_id, outcome, finished_at).await
}
