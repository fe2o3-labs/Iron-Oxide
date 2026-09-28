//! The axum server: the Dioxus app (SSR, assets, server functions) plus custom routes.

pub mod config;
pub mod state;

use std::sync::Arc;

use dioxus::logger::tracing;
use dioxus::server::axum::{Extension, Router, http::StatusCode, routing::get};

pub use self::{config::Config, state::AppState};
use crate::ui::App;

/// Runs the server until the process exits.
///
/// `dioxus::serve` binds to the `IP` and `PORT` environment variables, which [`Config`] has
/// already validated (Dioxus itself silently falls back to 127.0.0.1:8080 on a bad value).
/// In debug builds the closure runs again on every hot-patch, so the state is built only once.
pub fn serve(config: Config) -> ! {
    let config = Arc::new(config);
    dioxus::serve(move || {
        let config = Arc::clone(&config);
        async move {
            tracing::info!(
                bind_addr = %config.bind_addr,
                app_base_url = %config.app_base_url,
                database = config.database_url.redacted(),
                log_filter = config.log_filter.as_deref().unwrap_or("(default)"),
                auth_configured = config.auth.is_some(),
                "configuration loaded"
            );
            Ok(router(AppState::new(config)))
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

/// Routes served by axum directly, outside of Dioxus.
fn custom_routes() -> Router {
    Router::new().route("/healthz", get(healthz))
}

/// Liveness probe for the load balancer. It does not touch the database.
async fn healthz() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let response = custom_routes()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = dioxus::server::axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn unknown_custom_route_is_404() {
        let response = custom_routes()
            .oneshot(Request::get("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
