//! Every write of a workout session goes through this file, one function per write.
//!
//! Today each one calls its server function directly. The offline outbox (#30) will replace their
//! bodies, so that switching to it changes this file only. Callers generate the ids with
//! `new_v7()` and pass the client's clock ([`now`]), so a retried write is idempotent (see
//! `docs/api.md`).
//!
//! The home screen (#27) adds [`start_session`]; the session screen (#28) adds the others (save a
//! set, finish).

use dioxus::prelude::*;
use iron_oxide_domain::{SessionId, time::Timestamp};

use crate::api::sessions::{self, SessionView};

/// Starts a session of the active program on the next day of its rotation. `session_id` is new
/// for each attempt (`SessionId::new_v7()`); retrying the same attempt reuses it.
///
/// # Errors
/// The server function's: `409` when another session is in progress or no program is active.
pub async fn start_session(
    session_id: SessionId,
    started_at: Timestamp,
) -> Result<SessionView, ServerFnError> {
    sessions::start_session(session_id, started_at).await
}

/// The client's clock, for the timestamps the writes carry.
#[must_use]
pub fn now() -> Timestamp {
    #[cfg(feature = "web")]
    {
        // Milliseconds since the epoch, a whole number well within i64.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Date.now() is a whole number of ms, far below i64::MAX"
        )]
        Timestamp::from_epoch_millis(js_sys::Date::now() as i64)
    }
    #[cfg(not(feature = "web"))]
    {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        Timestamp::from_epoch_millis(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}
