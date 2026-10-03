//! Postgres: the connection pool, embedded migrations and the health check query.
//!
//! Pool settings follow the Neon decision (#39): a small pool, connections recycled well before
//! Neon's idle limits, no connections kept open while idle (so the compute can scale to zero),
//! and a retried first connection, because a suspended Neon compute takes a moment to wake up.

use std::time::Duration;

use dioxus::logger::tracing;
use sqlx::{
    PgPool,
    migrate::{MigrateError, Migrator},
    postgres::PgPoolOptions,
};

use super::config::DatabaseUrl;

pub mod account;
// The repository: typed queries over the training tables. Every function that reads or writes a
// user's data takes the caller's `UserId` and scopes every query by it (docs/database.md).
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod active_program;
pub mod error;
pub mod history;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod ids;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod programs;
#[cfg(test)]
mod schema_tests;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod sessions;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod sets;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod settings;
#[cfg(test)]
pub(crate) mod testing;
#[allow(dead_code, reason = "called by the server functions of #18-#22")]
pub mod training_maxes;
pub mod users;

/// The migrations in `crates/iron-oxide-app/migrations`, embedded in the binary at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Maximum number of pooled connections.
const MAX_CONNECTIONS: u32 = 5;
/// How long a request waits for a free connection (or a new one) before failing.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections are closed after this long, whatever their state (Neon: under 10 minutes).
const MAX_LIFETIME: Duration = Duration::from_secs(5 * 60);
/// Idle connections are closed after this long (Neon: under 5 minutes).
const IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// How long `/readyz` waits for `SELECT 1`.
pub const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// How the first connection is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total number of attempts, including the first one. At least 1.
    pub attempts: u32,
    /// How long one attempt may take. sqlx keeps retrying transient errors within an attempt.
    pub attempt_timeout: Duration,
    /// Delay before the second attempt; it doubles after each failure.
    pub initial_delay: Duration,
    /// Upper bound for a single delay.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Startup policy: 6 attempts of up to 5 s each, with 0.5 + 1 + 2 + 4 + 8 s between them
    /// (under a minute in the worst case). A waking Neon compute needs well under a second.
    pub const STARTUP: Self = Self {
        attempts: 6,
        attempt_timeout: Duration::from_secs(5),
        initial_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(8),
    };

    /// The delay after failed attempt number `failed_attempt` (1-based).
    pub fn delay_after(&self, failed_attempt: u32) -> Duration {
        let factor = 2_u32.saturating_pow(failed_attempt.saturating_sub(1));
        self.initial_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

/// Why the database could not be reached or prepared at startup.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("DATABASE_URL cannot be turned into connection options: {0}")]
    Options(#[source] sqlx::Error),
    #[error("could not connect to Postgres after {attempts} attempt(s): {source}")]
    Connect {
        attempts: u32,
        #[source]
        source: sqlx::Error,
    },
    #[error("could not run the database migrations: {0}")]
    Migrate(#[from] MigrateError),
    #[error("a built-in program does not load: {0}")]
    Builtins(#[from] iron_oxide_domain::program::BuiltinProgramError),
    #[error("could not seed the built-in programs: {0}")]
    Seed(#[source] error::RepoError),
}

/// Pool options shared by the app and the tests.
fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .min_connections(0)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .max_lifetime(MAX_LIFETIME)
        .idle_timeout(IDLE_TIMEOUT)
        // Neon closes connections of a suspended compute: check before handing one out.
        .test_before_acquire(true)
}

/// Creates the pool and checks that a first connection can be opened, retrying with exponential
/// backoff.
pub async fn connect(url: &DatabaseUrl, retry: RetryPolicy) -> Result<PgPool, DbError> {
    let options = url
        .connect_options()
        .map_err(DbError::Options)?
        .application_name("iron-oxide");
    let pool = pool_options().connect_lazy_with(options);
    let attempts = retry.attempts.max(1);
    let mut attempt = 1;
    loop {
        let result = match tokio::time::timeout(retry.attempt_timeout, pool.acquire()).await {
            Ok(result) => result.map(drop),
            Err(_) => Err(sqlx::Error::PoolTimedOut),
        };
        match result {
            Ok(()) => {
                tracing::info!(database = url.redacted(), attempt, "connected to Postgres");
                return Ok(pool);
            }
            Err(source) if attempt >= attempts => {
                return Err(DbError::Connect { attempts, source });
            }
            Err(error) => {
                let delay = retry.delay_after(attempt);
                tracing::warn!(
                    database = url.redacted(),
                    attempt,
                    %error,
                    retry_in_ms = delay.as_millis(),
                    "cannot connect to Postgres yet"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Applies the pending embedded migrations. Safe to run on every start: applied ones are skipped,
/// and a Postgres advisory lock serialises concurrent runs (which is why the Neon endpoint must be
/// the direct one, not the transaction pooler).
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    tracing::info!("database migrations are up to date");
    Ok(())
}

/// Why [`ping`] failed.
#[derive(Debug, thiserror::Error)]
pub enum PingError {
    #[error("the database did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the database query failed: {0}")]
    Query(#[from] sqlx::Error),
}

/// Runs `SELECT 1`, failing if it takes longer than `timeout`.
pub async fn ping(pool: &PgPool, timeout: Duration) -> Result<(), PingError> {
    let query = sqlx::query_scalar!(r#"SELECT 1 AS "one!""#).fetch_one(pool);
    match tokio::time::timeout(timeout, query).await {
        Ok(result) => result.map(|_| ()).map_err(PingError::from),
        Err(_) => Err(PingError::Timeout(timeout)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sqlx::{postgres::PgConnectOptions, types::Uuid};

    /// A pool whose server does not exist: nothing listens on port 1.
    pub(crate) fn unreachable_pool() -> PgPool {
        let options = PgConnectOptions::new().host("127.0.0.1").port(1);
        pool_options()
            .acquire_timeout(Duration::from_millis(500))
            .connect_lazy_with(options)
    }

    fn unreachable_url() -> DatabaseUrl {
        DatabaseUrl::parse("postgres://iron_oxide:pw@127.0.0.1:1/iron_oxide").unwrap()
    }

    #[test]
    fn startup_retry_doubles_and_is_capped() {
        let policy = RetryPolicy::STARTUP;
        let delays: Vec<Duration> = (1..=6).map(|n| policy.delay_after(n)).collect();
        assert_eq!(
            delays,
            [500, 1_000, 2_000, 4_000, 8_000, 8_000].map(Duration::from_millis)
        );
        let total: Duration = (1..policy.attempts).map(|n| policy.delay_after(n)).sum();
        assert_eq!(total, Duration::from_millis(15_500));
    }

    #[test]
    fn delay_does_not_overflow_on_huge_attempt_numbers() {
        let policy = RetryPolicy::STARTUP;
        assert_eq!(policy.delay_after(u32::MAX), policy.max_delay);
        assert_eq!(policy.delay_after(0), policy.initial_delay);
    }

    #[tokio::test]
    async fn connect_gives_up_after_the_last_attempt() {
        let retry = RetryPolicy {
            attempts: 3,
            attempt_timeout: Duration::from_millis(200),
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        };
        let error = connect(&unreachable_url(), retry).await.unwrap_err();
        assert!(
            matches!(error, DbError::Connect { attempts: 3, .. }),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains("after 3 attempt(s)"), "{message}");
        assert!(!message.contains("pw@"), "{message}");
    }

    #[tokio::test]
    async fn connect_makes_at_least_one_attempt() {
        let retry = RetryPolicy {
            attempts: 0,
            attempt_timeout: Duration::from_millis(200),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        };
        let error = connect(&unreachable_url(), retry).await.unwrap_err();
        assert!(
            matches!(error, DbError::Connect { attempts: 1, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn ping_fails_when_the_database_is_unreachable() {
        let error = ping(&unreachable_pool(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(error, PingError::Query(_)), "{error}");
    }

    #[tokio::test]
    async fn ping_times_out() {
        let error = ping(&unreachable_pool(), Duration::ZERO).await.unwrap_err();
        assert!(
            matches!(error, PingError::Timeout(Duration::ZERO)),
            "{error}"
        );
    }

    // --- Against Postgres: run with `cargo test -p iron-oxide-app --features server -- --ignored`
    // and DATABASE_URL pointing at the compose test database (see README). `sqlx::test` creates a
    // fresh database for each test and drops it afterwards.

    #[sqlx::test(migrations = false)]
    #[ignore = "needs Postgres"]
    async fn migrations_apply_to_an_empty_database_and_are_idempotent(pool: PgPool) {
        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();
        let applied: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(applied, i64::try_from(MIGRATOR.iter().count()).unwrap());
        ping(&pool, PING_TIMEOUT).await.unwrap();
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn users_get_defaults(pool: PgPool) {
        let (id, plan, display_name, age): (Uuid, String, Option<String>, f64) = sqlx::query_as(
            "INSERT INTO users DEFAULT VALUES \
                 RETURNING id, plan::text, display_name, \
                 extract(epoch FROM now() - created_at)::float8",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!id.is_nil());
        assert_eq!(plan, "free");
        assert_eq!(display_name, None);
        assert!((0.0..60.0).contains(&age), "{age}");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn users_plan_is_free_or_pro_only(pool: PgPool) {
        for plan in ["free", "pro"] {
            sqlx::query("INSERT INTO users (plan) VALUES ($1::user_plan)")
                .bind(plan)
                .execute(&pool)
                .await
                .unwrap();
        }
        let error = sqlx::query("INSERT INTO users (plan) VALUES ($1::user_plan)")
            .bind("gold")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("invalid input value for enum"),
            "{error}"
        );
        let null_plan = sqlx::query("INSERT INTO users (plan) VALUES (NULL)")
            .execute(&pool)
            .await;
        assert!(null_plan.is_err());
    }

    #[tokio::test]
    #[ignore = "needs Postgres"]
    async fn connect_and_ping_a_live_database() {
        let url = DatabaseUrl::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
        let pool = connect(&url, RetryPolicy::STARTUP).await.unwrap();
        ping(&pool, PING_TIMEOUT).await.unwrap();
    }
}
