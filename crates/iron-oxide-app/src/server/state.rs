//! Shared server state, reachable from every server function and custom route.
//!
//! [`crate::server::router`] adds the state to every request as an axum `Extension`, including
//! the server-function routes Dioxus registers. A server function declares it as an extra
//! server-only argument after the route; the client-side signature does not change:
//!
//! ```rust,ignore
//! // Server-only imports: the client build has no `server` module and no axum.
//! #[cfg(feature = "server")]
//! use {crate::server::AppState, dioxus::server::axum::Extension};
//!
//! #[get("/api/me", state: Extension<AppState>)]
//! pub async fn me() -> Result<Profile, ServerFnError> {
//!     let pool: &sqlx::PgPool = &state.db;
//!     // ... query with `pool`, scoped to the signed-in user.
//! }
//! ```
//!
//! Dioxus routes carry their own axum state (`FullstackContext`), so `State<AppState>` cannot be
//! used in server functions; `Extension<AppState>` is the supported way.

use std::sync::Arc;

use sqlx::PgPool;

use super::{
    config::Config,
    db::{self, DbError, RetryPolicy},
};

/// Everything the server shares between requests. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The validated configuration, loaded once at startup.
    #[allow(dead_code, reason = "read by the server functions of #5 and later")]
    pub config: Arc<Config>,
    /// The Postgres connection pool.
    pub db: PgPool,
}

impl AppState {
    /// Connects to Postgres (retrying while a cold Neon compute wakes up), applies the pending
    /// migrations and upserts the built-in programs.
    pub async fn init(config: Arc<Config>) -> Result<Self, DbError> {
        let db = db::connect(&config.database_url, RetryPolicy::STARTUP).await?;
        db::migrate(&db).await?;
        db::programs::seed_builtins(&db, db::programs::BUILTIN_PROGRAMS)
            .await
            .map_err(DbError::Seed)?;
        Ok(Self { config, db })
    }
}
