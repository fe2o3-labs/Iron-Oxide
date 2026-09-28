//! One error body for every server function (#68).
//!
//! Dioxus 0.7 answers a failed server function in two shapes:
//!
//! - an error returned by the function's body: `{"message", "code", "data"}`, where `message` is
//!   the `Display` of the `ServerFnError` (`error running server function: Not found. (details:
//!   None)`) and `data.ServerError.message` the message we wrote;
//! - an extractor rejection (`AuthUser`'s 401) or a request Dioxus could not decode (malformed or
//!   missing arguments): `{"error": text}`. Bad arguments come out as a `500` whose text is a serde
//!   error.
//!
//! This layer rewrites both, for `/api/` routes only, into the first shape with the clean message
//! on top: `{"message": m, "code": c, "data": {"ServerError": {"message": m, "code": c, ...}}}`.
//! The Dioxus client then decodes every failure as `ServerFnError::ServerError { message: m, code:
//! c, details }`. Arguments that do not decode become `422 Invalid request.`, and any other
//! `{"error"}` 500 the generic message; the original text is logged.

use dioxus::logger::tracing;
use dioxus::server::axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::Response,
};
use serde_json::{Value, json};

use super::error::INTERNAL;

/// The message of a request whose arguments do not decode.
pub const INVALID_REQUEST: &str = "Invalid request.";

/// Error bodies are small; anything bigger is passed through untouched.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// The prefixes of Dioxus's own texts for arguments it could not decode.
const BAD_ARGUMENTS: [&str; 3] = [
    "error deserializing server function",
    "missing argument",
    "error deserializing request",
];

/// The axum middleware: see the module documentation.
pub async fn normalize(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let response = next.run(request).await;
    let status = response.status();
    if !is_api || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_ERROR_BODY).await else {
        tracing::error!(%status, "server function error body too large or unreadable");
        return rebuild(parts, status, INTERNAL, None);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    match rewrite(status, &value) {
        Some((status, message, data)) => {
            parts.headers.remove(header::CONTENT_LENGTH);
            rebuild(parts, status, &message, data)
        }
        None => Response::from_parts(parts, Body::from(bytes)),
    }
}

/// The status, message and `data.ServerError` of the rewritten body, or `None` to keep it.
fn rewrite(status: StatusCode, body: &Value) -> Option<(StatusCode, String, Option<Value>)> {
    // Returned by the function's body: keep everything, put the clean message on top.
    if let Some(inner) = body.get("data").and_then(|data| data.get("ServerError")) {
        let message = inner.get("message")?.as_str()?.to_owned();
        return Some((status, message, Some(inner.clone())));
    }
    let text = body.get("error")?.as_str()?;
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        if BAD_ARGUMENTS.iter().any(|prefix| text.starts_with(prefix)) {
            tracing::info!(error = text, "server function arguments do not decode");
            return Some((
                StatusCode::UNPROCESSABLE_ENTITY,
                INVALID_REQUEST.to_owned(),
                None,
            ));
        }
        tracing::error!(error = text, "server function failed outside its body");
        return Some((status, INTERNAL.to_owned(), None));
    }
    Some((status, text.to_owned(), None))
}

fn rebuild(
    mut parts: dioxus::server::axum::http::response::Parts,
    status: StatusCode,
    message: &str,
    data: Option<Value>,
) -> Response {
    let code = status.as_u16();
    let mut inner = data.unwrap_or_else(|| json!({}));
    if let Some(object) = inner.as_object_mut() {
        object.insert("message".to_owned(), json!(message));
        object.insert("code".to_owned(), json!(code));
    }
    let body = json!({ "message": message, "code": code, "data": { "ServerError": inner } });
    parts.status = status;
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_error_gets_its_clean_message_on_top() {
        let body = json!({
            "message": "error running server function: Not found. (details: None)",
            "code": 404,
            "data": { "ServerError": { "message": "Not found.", "code": 404 } }
        });
        let (status, message, data) = rewrite(StatusCode::NOT_FOUND, &body).unwrap();
        assert_eq!(
            (status, message.as_str()),
            (StatusCode::NOT_FOUND, "Not found.")
        );
        assert_eq!(data, Some(json!({ "message": "Not found.", "code": 404 })));
    }

    #[test]
    fn details_are_kept() {
        let body = json!({
            "message": "x",
            "code": 422,
            "data": { "ServerError": { "message": "Invalid.", "code": 422, "details": [1, 2] } }
        });
        let (_, _, data) = rewrite(StatusCode::UNPROCESSABLE_ENTITY, &body).unwrap();
        assert_eq!(data.unwrap()["details"], json!([1, 2]));
    }

    #[test]
    fn an_extractor_rejection_keeps_its_status_and_message() {
        let body = json!({ "error": "Please sign in." });
        let (status, message, data) = rewrite(StatusCode::UNAUTHORIZED, &body).unwrap();
        assert_eq!(
            (status, message.as_str(), data),
            (StatusCode::UNAUTHORIZED, "Please sign in.", None)
        );
    }

    #[test]
    fn arguments_that_do_not_decode_are_422_without_the_serde_text() {
        for text in [
            "error deserializing server function results: UUID parsing failed: invalid character",
            "error deserializing server function arguments: missing field `session_id`",
            "missing argument session_id",
        ] {
            let body = json!({ "error": text });
            let (status, message, _) = rewrite(StatusCode::INTERNAL_SERVER_ERROR, &body).unwrap();
            assert_eq!(
                (status, message.as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, INVALID_REQUEST)
            );
        }
    }

    #[test]
    fn other_raw_500s_get_the_generic_message() {
        let body = json!({ "error": "error creating response: at 10.0.0.3" });
        let (status, message, _) = rewrite(StatusCode::INTERNAL_SERVER_ERROR, &body).unwrap();
        assert_eq!(
            (status, message.as_str()),
            (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL)
        );
    }

    #[test]
    fn unknown_shapes_are_kept() {
        assert_eq!(
            rewrite(StatusCode::BAD_GATEWAY, &json!({ "other": 1 })),
            None
        );
        assert_eq!(rewrite(StatusCode::BAD_GATEWAY, &json!("text")), None);
    }
}
