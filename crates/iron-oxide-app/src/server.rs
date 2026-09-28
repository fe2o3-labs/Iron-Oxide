//! The axum server: the Dioxus app (SSR, assets, server functions) plus custom routes.

pub mod config;
pub mod db;
pub mod state;

use std::{process::ExitCode, sync::Arc};

use dioxus::logger::tracing;
use dioxus::server::axum::{self, Extension, Router, http::StatusCode, routing::get};
use tokio::net::TcpListener;

pub use self::{config::Config, state::AppState};
use crate::ui::App;

/// Why the server stopped with an error.
#[derive(Debug, thiserror::Error)]
enum ServeError {
    #[error("cannot start the async runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error(transparent)]
    Database(#[from] db::DbError),
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("the HTTP server failed: {0}")]
    Serve(#[source] std::io::Error),
}

/// Runs the server until SIGTERM or Ctrl-C, then shuts it down gracefully.
///
/// Startup: connect to Postgres (with retries) and apply the migrations, then listen on
/// `IP`:`PORT` (validated by [`Config`]). On a shutdown signal the server stops accepting
/// connections, lets in-flight requests finish, then closes the pool. Returns a failure exit
/// code if startup or serving fails.
///
/// This replaces `dioxus::serve`, which has no graceful shutdown. What it adds on top is only
/// server-side hot-patching (`dx serve --hotpatch`); RSX hot reload in the browser still works.
pub fn serve(config: Config) -> ExitCode {
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ServeError::Runtime)
        .and_then(|runtime| runtime.block_on(run(Arc::new(config))));
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
    // The same logger `dioxus::serve` sets up (honours RUST_LOG).
    dioxus::logger::initialize_default();
    tracing::info!(
        bind_addr = %config.bind_addr,
        app_base_url = %config.app_base_url,
        database = config.database_url.redacted(),
        log_filter = config.log_filter.as_deref().unwrap_or("(default)"),
        auth_configured = config.auth.is_some(),
        "configuration loaded"
    );

    let addr = config.bind_addr;
    let state = AppState::init(config).await?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(%addr, "listening");

    let served = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServeError::Serve);

    state.db.close().await;
    tracing::info!("shut down");
    served
}

/// Resolves on SIGTERM (what Fly and Docker send) or Ctrl-C.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received, finishing in-flight requests");
}

/// Full server router: the Dioxus application merged with the custom routes, with the shared
/// state attached to every request (server functions included).
pub fn router(state: AppState) -> Router {
    dioxus::server::router(App)
        .merge(custom_routes())
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
        let config = Config::from_lookup(|name| match name {
            "DATABASE_URL" => Ok("postgres://u:p@127.0.0.1:1/db".to_owned()),
            "APP_BASE_URL" => Ok("http://localhost:8080".to_owned()),
            _ => Err(std::env::VarError::NotPresent),
        })
        .unwrap();
        AppState {
            config: Arc::new(config),
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
