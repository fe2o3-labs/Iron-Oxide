//! The axum server: the Dioxus app (SSR, assets, server functions) plus custom routes.

pub mod config;
pub mod db;
pub mod state;

use std::sync::Arc;

use dioxus::logger::tracing;
use dioxus::server::axum::{Extension, Router, http::StatusCode, routing::get};
use tokio::sync::OnceCell;

pub use self::{config::Config, state::AppState};
use crate::ui::App;

/// Runs the server until the process exits.
///
/// `dioxus::serve` binds to the `IP` and `PORT` environment variables, which [`Config`] has
/// already validated (Dioxus itself silently falls back to 127.0.0.1:8080 on a bad value).
/// In debug builds the closure runs again on every hot-patch, so the state (pool, migrations) is
/// built only once. If the database stays unreachable or a migration fails, the process exits
/// with status 1.
pub fn serve(config: Config) -> ! {
    let config = Arc::new(config);
    let state = Arc::new(OnceCell::<AppState>::new());
    dioxus::serve(move || {
        let config = Arc::clone(&config);
        let state = Arc::clone(&state);
        async move {
            tracing::info!(
                bind_addr = %config.bind_addr,
                app_base_url = %config.app_base_url,
                database = config.database_url.redacted(),
                log_filter = config.log_filter.as_deref().unwrap_or("(default)"),
                auth_configured = config.auth.is_some(),
                "configuration loaded"
            );
            match state.get_or_try_init(|| AppState::init(config)).await {
                Ok(state) => Ok(router(state.clone())),
                Err(error) => {
                    tracing::error!(%error, "startup failed");
                    eprintln!("error: {error}");
                    std::process::exit(1);
                }
            }
        }
    })
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
    Router::new().route("/healthz", get(healthz))
}

/// Health check: `200 ok` when Postgres answers `SELECT 1` within [`db::PING_TIMEOUT`], otherwise
/// `503` with a generic body (the cause is logged, not returned).
async fn healthz(Extension(state): Extension<AppState>) -> (StatusCode, &'static str) {
    match db::ping(&state.db, db::PING_TIMEOUT).await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(error) => {
            tracing::warn!(%error, "health check failed");
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
    async fn healthz_is_503_when_the_database_is_unreachable() {
        let (status, body) = get(routes(db::tests::unreachable_pool()), "/healthz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, "database unavailable");
    }

    #[tokio::test]
    async fn healthz_without_state_is_a_server_error() {
        let (status, _) = get(custom_routes(), "/healthz").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn unknown_custom_route_is_404() {
        let (status, _) = get(routes(db::tests::unreachable_pool()), "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = false)]
    #[ignore = "needs Postgres"]
    async fn healthz_is_200_ok_when_the_database_answers(pool: PgPool) {
        let (status, body) = get(routes(pool), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }
}
