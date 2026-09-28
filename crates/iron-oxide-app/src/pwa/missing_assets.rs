//! Answers `404 Not Found` for unknown `/assets/…` paths.
//!
//! The Dioxus router answers every path it does not know with the server-rendered app page
//! (`200 text/html`), `/assets/…` included. For a hashed asset URL that the running build does
//! not have (a client from a newer or older deploy asking the wrong replica), that is wrong: the
//! browser, and the service worker's cache, would take the app page for the JS, CSS or wasm file.
//! A real asset is never HTML, so an HTML response under `/assets/` can only be that fallback.

use dioxus::server::axum::{
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// URL prefix of the hashed files that `dx` bundles.
const ASSETS_PREFIX: &str = "/assets/";

/// Axum middleware: replaces the SSR fallback page served under `/assets/` with a 404.
///
/// Apply it to the whole router with `axum::middleware::from_fn(missing_assets_are_not_found)`.
pub async fn missing_assets_are_not_found(request: Request, next: Next) -> Response {
    let is_asset_path = request.uri().path().starts_with(ASSETS_PREFIX);
    let response = next.run(request).await;
    if is_asset_path && is_html(&response) {
        StatusCode::NOT_FOUND.into_response()
    } else {
        response
    }
}

fn is_html(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("text/html")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::{
        Router, body::Body, http::HeaderValue, middleware::from_fn, routing::get,
    };
    use tower::ServiceExt;

    /// Mimics the Dioxus router: one known asset, and the HTML app page for every other path.
    fn app() -> Router {
        Router::new()
            .route(
                "/assets/app-dxh123.js",
                get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], "js") }),
            )
            .fallback(get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<html>",
                )
            }))
            .layer(from_fn(missing_assets_are_not_found))
    }

    async fn get_status(path: &str) -> (StatusCode, Option<HeaderValue>) {
        let response = app()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        (
            response.status(),
            response.headers().get(header::CONTENT_TYPE).cloned(),
        )
    }

    #[tokio::test]
    async fn unknown_asset_is_404() {
        let (status, _) = get_status("/assets/app-dxh999.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_nested_asset_is_404() {
        let (status, _) = get_status("/assets/sub/dir/file.wasm").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn known_asset_is_served() {
        let (status, content_type) = get_status("/assets/app-dxh123.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.unwrap(), "text/javascript");
    }

    #[tokio::test]
    async fn app_pages_still_get_the_html_fallback() {
        for path in ["/", "/history", "/assets", "/assetsx/app.js"] {
            let (status, content_type) = get_status(path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(content_type.unwrap(), "text/html; charset=utf-8", "{path}");
        }
    }

    #[test]
    fn html_detection_ignores_case_and_parameters() {
        let response = |value: &'static str| {
            let mut response = Response::new(Body::empty());
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            response
        };
        assert!(is_html(&response("text/html")));
        assert!(is_html(&response("Text/HTML; charset=utf-8")));
        assert!(!is_html(&response("text/javascript")));
        assert!(!is_html(&response("application/wasm")));
        assert!(!is_html(&Response::new(Body::empty())));
    }
}
