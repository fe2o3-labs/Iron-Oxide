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

use iron_oxide_domain::program::builtin_programs;
use sqlx::PgPool;
use tokio::sync::Semaphore;

use super::{
    config::Config,
    db::{self, DbError, RetryPolicy},
};

/// Everything the server shares between requests. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The validated configuration, loaded once at startup.
    #[allow(dead_code, reason = "read by later server functions")]
    pub config: Arc<Config>,
    /// The Postgres connection pool.
    pub db: PgPool,
    /// The account imports and deletions this process runs at once (#22), see
    /// `server::api::account::MAX_ACCOUNT_OPERATIONS`.
    pub account_slots: Arc<Semaphore>,
}

impl AppState {
    /// Connects to Postgres (retrying while a cold Neon compute wakes up), applies the pending
    /// migrations and upserts the built-in programs.
    pub async fn init(config: Arc<Config>) -> Result<Self, DbError> {
        let db = db::connect(&config.database_url, RetryPolicy::STARTUP).await?;
        db::migrate(&db).await?;
        let builtins = builtin_programs()?;
        db::programs::seed_builtins(&db, &db::programs::builtin_seeds(&builtins))
            .await
            .map_err(DbError::Seed)?;
        Ok(Self::new(config, db))
    }

    /// The state over an existing pool.
    #[must_use]
    pub fn new(config: Arc<Config>, db: PgPool) -> Self {
        Self {
            config,
            db,
            account_slots: Arc::new(Semaphore::new(super::api::account::MAX_ACCOUNT_OPERATIONS)),
        }
    }
}
