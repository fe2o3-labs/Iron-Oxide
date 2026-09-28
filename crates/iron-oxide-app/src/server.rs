//! The axum server: the Dioxus app (SSR, assets, server functions) plus custom routes.

pub mod auth;
pub mod billing;
pub mod config;
pub mod db;
pub mod dotenv;
pub mod entitlements;
pub mod logging;
pub mod state;

use std::{future::IntoFuture, process::ExitCode, sync::Arc, time::Duration};

use dioxus::logger::tracing;
use dioxus::server::axum::{
    self, Extension, Router, http::StatusCode, middleware::from_fn, routing::get,
};
use tokio::net::TcpListener;

pub use self::{config::Config, state::AppState};
use crate::pwa::missing_assets::missing_assets_are_not_found;
use crate::ui::App;

/// Why the server stopped with an error.
#[derive(Debug, thiserror::Error)]
enum ServeError {
    #[error("cannot start the async runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    Database(#[from] db::DbError),
    #[error("cannot set up sign-in: {0}")]
    Auth(#[source] auth::AuthError),
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot install the shutdown signal handlers: {0}")]
    Signals(#[source] std::io::Error),
    #[error("the HTTP server failed: {0}")]
    Serve(#[source] std::io::Error),
    #[error("in-flight requests did not finish within the {0:?} shutdown grace period")]
    DrainTimeout(Duration),
    #[error("stopped immediately on a second shutdown signal")]
    Forced,
}

/// How long closing the connection pool may take once serving has stopped.
const POOL_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Runs the server until SIGINT/Ctrl-C or SIGTERM, then shuts it down gracefully.
///
/// Startup: connect to Postgres (with retries) and apply the migrations, then listen on
/// `IP`:`PORT` (validated by [`Config`]). On a shutdown signal the server stops accepting
/// connections and lets in-flight requests finish, for at most `SHUTDOWN_GRACE_SECS`
/// (default 20 s); a second signal stops it at once. Then it closes the pool and exits: status 0
/// after a clean drain, 1 if startup or serving failed or the drain was cut short.
///
/// This replaces `dioxus::serve`, which has no graceful shutdown. What it adds on top is only
/// server-side hot-patching (`dx serve --hotpatch`); RSX hot reload in the browser still works.
pub fn serve(config: Config) -> ExitCode {
    let result = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let result = runtime.block_on(run(Arc::new(config)));
            // Connection tasks cut off by the grace period may still be parked: do not wait.
            runtime.shutdown_background();
            result
        }
        Err(error) => Err(ServeError::Runtime(error)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "server stopped with an error");
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: Arc<Config>) -> Result<(), ServeError> {
    // The same output as the Dioxus logger (honours RUST_LOG), with sign-in material capped.
    logging::init(config.log_filter.as_deref());
    tracing::info!(
        bind_addr = %config.bind_addr,
        app_base_url = %config.app_base_url,
        database = config.database_url.redacted(),
        log_filter = config.log_filter.as_deref().unwrap_or("(default)"),
        shutdown_grace_secs = config.shutdown_grace.as_secs(),
        cookie_secure = config.auth.cookie_secure,
        stripe_webhook_secret_set = config.billing.stripe_webhook_secret.is_some(),
        "configuration loaded"
    );

    let addr = config.bind_addr;
    let grace = config.shutdown_grace;
    // Sign-in (#5): built before touching the database, so a bad setting fails fast.
    let auth = auth::AuthState::new(&config).map_err(ServeError::Auth)?;
    let state = AppState::init(config).await?;
    let cleanup = auth::spawn_cleanup(state.db.clone());
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    let mut signals = ShutdownSignals::install().map_err(ServeError::Signals)?;
    tracing::info!(%addr, "listening");

    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, router(state.clone(), auth))
        .with_graceful_shutdown(async {
            // Resolves when told to drain (or if the sender is dropped).
            let _ = drain_rx.await;
        })
        .into_future();
    tokio::pin!(server);

    let served = tokio::select! {
        result = &mut server => result.map_err(ServeError::Serve),
        signal = signals.next() => {
            tracing::info!(
                signal,
                grace_secs = grace.as_secs(),
                "shutdown signal received, finishing in-flight requests"
            );
            let _ = drain_tx.send(());
            tokio::select! {
                result = &mut server => result.map_err(ServeError::Serve),
                () = tokio::time::sleep(grace) => {
                    tracing::warn!(
                        grace_secs = grace.as_secs(),
                        "requests still in flight after the grace period, closing them"
                    );
                    Err(ServeError::DrainTimeout(grace))
                }
                signal = signals.next() => {
                    tracing::warn!(signal, "second shutdown signal, stopping now");
                    Err(ServeError::Forced)
                }
            }
        }
    };

    cleanup.abort();
    if tokio::time::timeout(POOL_CLOSE_TIMEOUT, state.db.close())
        .await
        .is_err()
    {
        tracing::warn!("the database pool did not close in time");
    }
    tracing::info!("shut down");
    served
}

/// The signals that stop the server: SIGINT (Ctrl-C, and Fly's default kill signal) and SIGTERM
/// (Docker's, and what `fly.toml` sets). Handlers stay installed, so a second signal is seen too.
struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Waits for the next signal and returns its name.
    async fn next(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                Some(()) = self.interrupt.recv() => "SIGINT",
                Some(()) = self.terminate.recv() => "SIGTERM",
                else => std::future::pending().await,
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => "Ctrl-C",
                Err(error) => {
                    tracing::warn!(%error, "cannot listen for Ctrl-C");
                    std::future::pending().await
                }
            }
        }
    }
}

/// Full server router: the Dioxus application merged with the custom routes, with the shared
/// state attached to every request (server functions included).
pub fn router(state: AppState, auth: auth::AuthState) -> Router {
    let app = dioxus::server::router(App)
        .merge(custom_routes())
        .layer(from_fn(missing_assets_are_not_found));
    // Sign-in (#5): sessions, the CSRF check and the Google callback around the app.
    auth::install(app, auth, state.db.clone())
        // Merged after `auth::install`, so outside its session and CSRF layers: Stripe's webhook
        // deliveries are cross-site POSTs, authenticated by their signature (billing.rs).
        .merge(billing::routes())
        .layer(Extension(state))
}

/// Routes served by axum directly, outside of Dioxus. They read [`AppState`] from the
/// `Extension` added by [`router`].
fn custom_routes() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// Liveness: `200 ok` while the process serves HTTP. It never touches the database, so the
/// platform can poll it often without keeping a scale-to-zero Neon compute awake.
async fn healthz() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

/// Readiness: `200 ok` when Postgres answers `SELECT 1` within [`db::PING_TIMEOUT`], otherwise
/// `503` with a generic body (the cause is logged, never returned).
async fn readyz(Extension(state): Extension<AppState>) -> (StatusCode, &'static str) {
    match db::ping(&state.db, db::PING_TIMEOUT).await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::{
        body::{Body, to_bytes},
        http::Request,
        response::Response,
    };
    use sqlx::PgPool;
    use tower::ServiceExt;

    fn state(db: PgPool) -> AppState {
        AppState {
            config: Arc::new(auth::test_support::config()),
            db,
        }
    }

    async fn get(router: Router, path: &str) -> (StatusCode, String) {
        let response: Response = router
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn routes(db: PgPool) -> Router {
        custom_routes().layer(Extension(state(db)))
    }

    #[tokio::test]
    async fn healthz_is_200_ok_without_touching_the_database() {
        // Neither a state nor a database: liveness must not need them.
        let (status, body) = get(custom_routes(), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
        let (status, body) = get(routes(db::tests::unreachable_pool()), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn readyz_is_503_when_the_database_is_unreachable() {
        let (status, body) = get(routes(db::tests::unreachable_pool()), "/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, "database unavailable");
    }

    #[tokio::test]
    async fn readyz_without_state_is_a_server_error() {
        let (status, _) = get(custom_routes(), "/readyz").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn unknown_custom_route_is_404() {
        let (status, _) = get(routes(db::tests::unreachable_pool()), "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = false)]
    #[ignore = "needs Postgres"]
    async fn readyz_is_200_ok_when_the_database_answers(pool: PgPool) {
        let (status, body) = get(routes(pool), "/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }
}
