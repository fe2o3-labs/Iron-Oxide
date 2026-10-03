//! Request limits (#74): a cap on every request body and a deadline for receiving it. Values and
//! contract: `docs/api.md`, "Request limits".
//!
//! - [`cap_body`] reads the whole body of every request that reaches Dioxus (server functions and
//!   pages) before Dioxus does, and refuses it with `413` past [`RequestLimits::body`]. Dioxus
//!   0.7.10 reads a server function's body with `unwrap` and panics when that fails (past axum's
//!   2 MiB default, a broken connection), so a limit that only fails the read would still end in
//!   a panic: the body must be complete and within the limit before Dioxus sees it. A body that
//!   does not arrive within [`RequestLimits::body_read_timeout`] is refused with `408`.
//!
//! There is no overall timeout on `/api/` calls: the server answers once the server function has
//! finished. An HTTP timeout could not stop it (Dioxus 0.7.10 runs it in a detached task), so a
//! `503` sent before the function ends would let a stale write commit after the client moved on.
//! Everything that can block is bounded where it happens instead: the body read (here, `408`),
//! the pool's acquire timeout (`503`), Postgres's statement and transaction deadlines
//! ([`crate::server::db::STATEMENT_DEADLINE`], `503`, rolled back) and the client timeout of
//! every outbound call (Google, `503`).
//!
//! Routes with a larger limit read their body themselves with [`read_body`] and are listed in
//! [`OWN_BODY_LIMIT`]: the program upload (`UPLOAD_BODY_LIMIT`, checked after the session so a
//! signed-out client gets its `401` without the body being read) and the account import
//! (`IMPORT_BODY_LIMIT`, likewise after the session and an import slot, with its own read
//! timeout, [`RequestLimits::import_body_read_timeout`]). The Stripe webhook is mounted
//! outside this layer with its own `DefaultBodyLimit`.

use std::time::Duration;

use dioxus::logger::tracing;
use dioxus::server::axum::{
    body::{Body, Bytes, HttpBody},
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header::CONTENT_LENGTH},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::api::errors_layer;

/// Largest request body any route reaches Dioxus with, unless it is in [`OWN_BODY_LIMIT`].
///
/// The largest legitimate server-function bodies are a few KiB (measured by
/// `the_default_body_limit_covers_every_server_function` in `server::limits::tests`): a passkey
/// registration, a settings update with a full plate inventory. 64 KiB leaves a wide margin.
pub const DEFAULT_BODY_LIMIT: usize = 64 * 1024;

/// How long a request body may take to arrive, counted from the end of the headers.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The paths that read their body themselves, with their own (larger) limit, through
/// [`read_body`]. [`cap_body`] leaves their body alone.
pub const OWN_BODY_LIMIT: [&str; 2] = [
    crate::api::programs::UPLOAD_PATH,
    crate::api::account::IMPORT_PATH,
];

/// The message of a `413` from the default limit.
pub const TOO_LARGE: &str = "This request is too large.";
/// The message of a `408`: the body did not arrive in time.
pub const BODY_TIMEOUT: &str = "The request took too long to arrive. Please try again.";

/// The limits applied to requests. Built in (not configurable); tests use shorter ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLimits {
    /// Largest body, in bytes, of a request that reaches Dioxus.
    pub body: usize,
    /// How long a body may take to arrive.
    pub body_read_timeout: Duration,
    /// How long an account import's body may take to arrive (#22): longer, for its size.
    pub import_body_read_timeout: Duration,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            body: DEFAULT_BODY_LIMIT,
            body_read_timeout: BODY_READ_TIMEOUT,
            import_body_read_timeout: Duration::from_secs(
                crate::api::account::IMPORT_BODY_READ_TIMEOUT_SECS,
            ),
        }
    }
}

/// Why [`read_body`] did not return a body.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    /// Over the limit, announced by `Content-Length` or actually sent.
    #[error("body over {0} bytes")]
    TooLarge(usize),
    /// The body did not arrive within the read timeout.
    #[error("body not received within {0:?}")]
    TimedOut(Duration),
    /// The connection failed while the body was read (the client went away).
    #[error("body unreadable: {0}")]
    Broken(dioxus::server::axum::Error),
}

/// Reads the whole `body`, refusing it past `limit` bytes: at once when `Content-Length`
/// announces more, otherwise as soon as more has arrived (a chunked body, or a `Content-Length`
/// that lies). Gives up after `timeout`.
pub async fn read_body(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes, BodyError> {
    let announced = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if announced.is_some_and(|length| length > limit as u64) {
        return Err(BodyError::TooLarge(limit));
    }
    match tokio::time::timeout(timeout, collect(body, limit)).await {
        Ok(result) => result,
        Err(_) => Err(BodyError::TimedOut(timeout)),
    }
}

async fn collect(mut body: Body, limit: usize) -> Result<Bytes, BodyError> {
    let mut bytes = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.map_err(BodyError::Broken)?;
        if let Ok(data) = frame.into_data() {
            if bytes.len() + data.len() > limit {
                return Err(BodyError::TooLarge(limit));
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(Bytes::from(bytes))
}

/// The response to a body [`read_body`] refused: `413` with `too_large` as the message, `408`, or
/// `400` for a broken connection (nobody reads it). In the `/api/` error shape on `/api/` paths,
/// as plain text elsewhere.
pub fn refuse_body(error: &BodyError, is_api: bool, too_large: &str) -> Response {
    let (status, message) = match error {
        BodyError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, too_large),
        BodyError::TimedOut(_) => (StatusCode::REQUEST_TIMEOUT, BODY_TIMEOUT),
        BodyError::Broken(_) => (StatusCode::BAD_REQUEST, "Bad request."),
    };
    tracing::info!(%error, %status, "request body refused");
    if is_api {
        errors_layer::error_response(status, message, None)
    } else {
        (status, message.to_owned()).into_response()
    }
}

fn is_api(request: &Request) -> bool {
    request.uri().path().starts_with("/api/")
}

/// The body cap, around everything Dioxus serves: see the module documentation.
pub async fn cap_body(
    State(limits): State<RequestLimits>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    // Nothing to read (`GET`s, upgrades), or a route that reads its body itself.
    if request.body().is_end_stream() || OWN_BODY_LIMIT.contains(&path) {
        return next.run(request).await;
    }
    let is_api = is_api(&request);
    let (parts, body) = request.into_parts();
    match read_body(&parts.headers, body, limits.body, limits.body_read_timeout).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(error) => refuse_body(&error, is_api, TOO_LARGE),
    }
}

#[cfg(test)]
#[path = "limits/tests.rs"]
pub(crate) mod tests;
