//! The axum server: the Dioxus app (SSR, assets, server functions) plus custom routes.

use dioxus::server::axum::{Router, http::StatusCode, middleware::from_fn, routing::get};

use crate::pwa::missing_assets::missing_assets_are_not_found;
use crate::ui::App;

/// Full server router: the Dioxus application merged with the custom routes.
pub fn router() -> Router {
    dioxus::server::router(App)
        .merge(custom_routes())
        .layer(from_fn(missing_assets_are_not_found))
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
