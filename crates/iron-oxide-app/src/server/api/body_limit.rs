//! Capping the body of a server function that takes an upload (a `program.json`, an account
//! export), before anything reads it.

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;
use dioxus::server::axum::{
    Json,
    body::{Body, to_bytes},
    extract::{FromRequestParts, Request},
    http::{StatusCode, header::CONTENT_LENGTH},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use super::ApiError;
use crate::server::auth::AuthUser;

/// The body of a middleware on an upload route: checks the session first, so a signed-out client
/// gets its `401` without the server reading (up to `limit` bytes of) its body. Then reads the
/// whole body before the server function does, and refuses it with `413` (`too_large`'s message)
/// past `limit`, announced (`Content-Length`) or actually sent.
///
/// Dioxus reads a server function's body itself and panics when that fails (axum's 2 MiB default
/// limit, a broken connection), so the body it gets here is already complete and within the limit.
/// A route whose limit is above 2 MiB also needs `DefaultBodyLimit::max(limit)`. The server
/// function's own `AuthUser` looks the session up again; that is the price of refusing early.
pub async fn signed_in_and_capped(
    request: Request,
    next: Next,
    limit: usize,
    too_large: fn() -> ApiError,
) -> Response {
    let (mut parts, body) = request.into_parts();
    if let Err(rejection) = AuthUser::from_request_parts(&mut parts, &()).await {
        return rejection;
    }
    let announced = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if announced.is_some_and(|length| length > limit as u64) {
        return refuse(too_large(), "announced body too large");
    }
    match to_bytes(body, limit).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        // Over the limit, or unreadable (the client went away: nobody reads the answer).
        Err(error) => {
            tracing::info!(%error, path = %parts.uri.path(), "upload body not read");
            refuse(too_large(), "body too large or unreadable")
        }
    }
}

/// A `413`, in the shape Dioxus gives the errors a server function returns (so the client decodes
/// it the same way): `{"message", "code", "data": <the ServerFnError>}`.
fn refuse(error: ApiError, reason: &'static str) -> Response {
    tracing::info!(reason, "upload refused");
    let error = ServerFnError::from(error);
    let body = json!({ "message": error.to_string(), "code": 413, "data": error });
    (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response()
}
