//! The client side of the rate-limit contract (#23). The limits themselves are enforced by the
//! server (`crate::server::rate_limit`); see `docs/rate-limiting.md`.
//!
//! A refused call answers `429 Too Many Requests` with:
//! - a `Retry-After` header: whole seconds, rounded up, at least 1;
//! - for server functions, the server-function error body
//!   `{"message", "code": 429, "data": {"ServerError": {"message", "code": 429, "details":
//!   {"retry_after_secs": N}}}}` (`docs/api.md`), the same `N`. The client decodes it as
//!   `ServerFnError::ServerError { code: 429, details: Some({"retry_after_secs": N}), .. }`.
//!
//! A 429 is retryable, but only after the delay: never sooner.

// Read by the client retry queue (#30) and the error classification (#68), not yet written.
#![cfg_attr(not(test), allow(dead_code))]

use std::time::Duration;

use dioxus::prelude::ServerFnError;

/// The key of the delay in seconds in a 429 error's `details`.
pub const RETRY_AFTER_SECS: &str = "retry_after_secs";

/// The delay used when a 429 carries none (it reached the client without its body).
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(30);

/// When `error` is a 429 (rate limited), how long to wait before retrying; `None` for any other
/// error.
#[must_use]
pub fn retry_after(error: &ServerFnError) -> Option<Duration> {
    match error {
        ServerFnError::ServerError {
            code: 429, details, ..
        } => Some(
            details
                .as_ref()
                .and_then(|details| details.get(RETRY_AFTER_SECS))
                .and_then(serde_json::Value::as_u64)
                .filter(|secs| *secs > 0)
                .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs),
        ),
        ServerFnError::Request(dioxus::fullstack::RequestError::Status(_, 429)) => {
            Some(DEFAULT_RETRY_AFTER)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::fullstack::RequestError;
    use serde_json::json;

    fn server_error(code: u16, details: Option<serde_json::Value>) -> ServerFnError {
        ServerFnError::ServerError {
            message: "x".to_owned(),
            code,
            details,
        }
    }

    #[test]
    fn a_429_with_a_delay_waits_that_long() {
        let error = server_error(429, Some(json!({ RETRY_AFTER_SECS: 17 })));
        assert_eq!(retry_after(&error), Some(Duration::from_secs(17)));
    }

    #[test]
    fn a_429_without_a_usable_delay_waits_the_default() {
        for details in [
            None,
            Some(json!({})),
            Some(json!({ RETRY_AFTER_SECS: 0 })),
            Some(json!({ RETRY_AFTER_SECS: -3 })),
            Some(json!({ RETRY_AFTER_SECS: "soon" })),
            Some(json!("not an object")),
        ] {
            let error = server_error(429, details.clone());
            assert_eq!(
                retry_after(&error),
                Some(DEFAULT_RETRY_AFTER),
                "{details:?}"
            );
        }
        let bare = ServerFnError::Request(RequestError::Status("x".to_owned(), 429));
        assert_eq!(retry_after(&bare), Some(DEFAULT_RETRY_AFTER));
    }

    #[test]
    fn other_errors_are_not_rate_limits() {
        let details = Some(json!({ RETRY_AFTER_SECS: 5 }));
        assert_eq!(retry_after(&server_error(503, details)), None);
        assert_eq!(retry_after(&server_error(401, None)), None);
        assert_eq!(retry_after(&ServerFnError::new("x")), None);
        let bare = ServerFnError::Request(RequestError::Status("x".to_owned(), 500));
        assert_eq!(retry_after(&bare), None);
    }
}
