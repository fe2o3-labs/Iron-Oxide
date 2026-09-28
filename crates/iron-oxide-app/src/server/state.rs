//! Shared server state, reachable from every server function and custom route.
//!
//! [`crate::server::router`] adds the state to every request as an axum `Extension`, including
//! the server-function routes Dioxus registers. A server function declares it as an extra
//! server-only argument after the route; the client-side signature does not change:
//!
//! ```rust,ignore
//! use dioxus::fullstack::extract::Extension; // or dioxus::server::axum::Extension
//! use crate::server::AppState;
//!
//! #[get("/api/me", state: Extension<AppState>)]
//! pub async fn me() -> Result<Profile, ServerFnError> {
//!     let pool = &state.db;
//!     // ...
//! }
//! ```
//!
//! Dioxus routes carry their own axum state (`FullstackContext`), so `State<AppState>` cannot be
//! used in server functions; `Extension<AppState>` is the supported way.

use std::sync::Arc;

use super::config::Config;

/// Everything the server shares between requests. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The validated configuration, loaded once at startup.
    #[allow(dead_code, reason = "read by the server functions of #5 and later")]
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}
