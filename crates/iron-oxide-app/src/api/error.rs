//! Client side of the error conventions (#68, `docs/api.md`): what a failed server function means
//! for the UI and the retry queue (#30).
//!
//! The server maps every failure to a status and a short message that is safe to show
//! (`crate::server::api::ApiError`), in one body shape for every `/api/` route
//! (`crate::server::api::errors_layer`). The Dioxus client decodes it as
//! `ServerFnError::ServerError { message, code, details }`: our message and our details are that
//! variant's own fields. A response that is not ours (a proxy, a gateway, a panic) arrives with
//! `message` = `HTTP {code}: {text}`, or as a bare `RequestError::Status(_, code)`.
//!
//! [`ApiFailure::classify`] handles all of them, plus network failures, which never reached the
//! server. It shows our message for 4xx and 503 answers and a generic one otherwise: never a
//! `HTTP …` text or the `Display` of the error.

use dioxus::fullstack::RequestError;
use dioxus::prelude::ServerFnError;
use serde_json::Value;

/// What kind of failure it was, from the user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// `401`: signed out or the session expired. Show the sign-in screen.
    Unauthorized,
    /// `403`: not allowed on the user's plan.
    Forbidden,
    /// `404`: the item does not exist (or is not the user's).
    NotFound,
    /// `409`: contradicts saved data. Retrying the same request cannot succeed.
    Conflict,
    /// `400`/`413`/`422`: the request was rejected as invalid (or too large). Retrying cannot
    /// succeed.
    Invalid,
    /// `503`, `502`, `504`: the server could not do it right now; nothing was saved. Retry.
    Transient,
    /// `429`: too many requests. Retry later, honouring `Retry-After`.
    RateLimited,
    /// The request never got an answer (offline, timeout, connection dropped), or the server
    /// gave up waiting for its body (`408`, a slow connection). Retry.
    Network,
    /// Anything else: a server bug (`500`) or a client-side encoding problem.
    Other,
}

impl FailureKind {
    /// Whether sending the exact same request again may succeed. Server functions are idempotent
    /// (client-generated ids), so a retry never duplicates anything.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Transient | Self::RateLimited | Self::Network)
    }

    fn from_status(code: u16) -> Self {
        match code {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            408 => Self::Network,
            409 => Self::Conflict,
            400 | 413 | 422 => Self::Invalid,
            429 => Self::RateLimited,
            502..=504 => Self::Transient,
            _ => Self::Other,
        }
    }
}

/// Shown when the server's message cannot be used (network failures, bare statuses, 5xx).
pub const GENERIC_MESSAGE: &str = "Something went wrong. Please try again.";
/// Shown when the server could not be reached.
pub const NETWORK_MESSAGE: &str = "Cannot reach the server. Check your connection.";
/// Shown for a `503` or a gateway error without a usable message.
pub const TRANSIENT_MESSAGE: &str = "The server is busy. Please try again.";

/// A failed server function call, classified for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiFailure {
    pub kind: FailureKind,
    /// The message to show the user.
    pub message: String,
    /// Structured details some errors carry for the UI (the list of problems in a program
    /// document, a 429's `retry_after_secs`).
    pub details: Option<Value>,
}

impl ApiFailure {
    /// Classifies `error`. The server's own message is kept for 4xx and 503 answers, which it
    /// writes for the user; anything else gets a generic message.
    #[must_use]
    pub fn classify(error: &ServerFnError) -> Self {
        match error {
            ServerFnError::ServerError {
                code,
                message,
                details,
            } => {
                let kind = FailureKind::from_status(*code);
                let not_ours = message.starts_with(&format!("HTTP {code}:"));
                let message = if kind == FailureKind::Other {
                    // A 500's message is generic on our server; anything else's is unknown.
                    GENERIC_MESSAGE.to_owned()
                } else if not_ours || message.trim().is_empty() {
                    default_message(kind).to_owned()
                } else {
                    message.clone()
                };
                Self {
                    kind,
                    message,
                    details: if not_ours { None } else { details.clone() },
                }
            }
            ServerFnError::Request(RequestError::Status(_, code)) => {
                Self::without_details(FailureKind::from_status(*code))
            }
            ServerFnError::Request(
                RequestError::Timeout(_)
                | RequestError::Request(_)
                | RequestError::Connect(_)
                | RequestError::Body(_),
            ) => Self::without_details(FailureKind::Network),
            _ => Self::without_details(FailureKind::Other),
        }
    }

    fn without_details(kind: FailureKind) -> Self {
        Self {
            kind,
            message: default_message(kind).to_owned(),
            details: None,
        }
    }

    /// For a `429`, how long to wait before retrying, from the details
    /// (`{"retry_after_secs": n}`, see `docs/api.md`).
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self.kind {
            FailureKind::RateLimited => self
                .details
                .as_ref()
                .and_then(|details| details.get("retry_after_secs"))
                .and_then(Value::as_u64),
            _ => None,
        }
    }

    /// Whether sending the exact same request again may succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}

impl From<&ServerFnError> for ApiFailure {
    fn from(error: &ServerFnError) -> Self {
        Self::classify(error)
    }
}

/// The message for a status that came without a usable one.
const fn default_message(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Unauthorized => "Please sign in.",
        FailureKind::Forbidden => "This is not available on your plan.",
        FailureKind::NotFound => "Not found.",
        FailureKind::Conflict => "This conflicts with data that is already saved.",
        FailureKind::Invalid => "Some values are not valid.",
        FailureKind::Transient => TRANSIENT_MESSAGE,
        FailureKind::RateLimited => "Too many requests. Please wait a moment.",
        FailureKind::Network => NETWORK_MESSAGE,
        FailureKind::Other => GENERIC_MESSAGE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure from our server as the Dioxus client decodes it (see
    /// `server::api::errors_layer::tests`, which goes through the real decoder).
    fn server(code: u16, message: &str) -> ServerFnError {
        ServerFnError::ServerError {
            message: message.to_owned(),
            code,
            details: None,
        }
    }

    fn status(code: u16) -> ServerFnError {
        ServerFnError::Request(RequestError::Status("x".to_owned(), code))
    }

    #[test]
    fn statuses_map_to_kinds_and_retryability() {
        let cases = [
            (401, FailureKind::Unauthorized, false),
            (403, FailureKind::Forbidden, false),
            (404, FailureKind::NotFound, false),
            (408, FailureKind::Network, true),
            (409, FailureKind::Conflict, false),
            (400, FailureKind::Invalid, false),
            (413, FailureKind::Invalid, false),
            (422, FailureKind::Invalid, false),
            (429, FailureKind::RateLimited, true),
            (500, FailureKind::Other, false),
            (502, FailureKind::Transient, true),
            (503, FailureKind::Transient, true),
            (504, FailureKind::Transient, true),
            (418, FailureKind::Other, false),
        ];
        for (code, kind, retryable) in cases {
            for error in [server(code, "m"), status(code)] {
                let failure = ApiFailure::classify(&error);
                assert_eq!(failure.kind, kind, "{code}");
                assert_eq!(failure.is_retryable(), retryable, "{code}");
            }
        }
    }

    #[test]
    fn our_message_is_shown_for_4xx_and_503_only() {
        assert_eq!(
            ApiFailure::classify(&server(409, "This session has already ended.")).message,
            "This session has already ended."
        );
        assert_eq!(
            ApiFailure::classify(&server(422, "Invalid request.")).message,
            "Invalid request."
        );
        assert_eq!(
            ApiFailure::classify(&server(500, "stack trace")).message,
            GENERIC_MESSAGE
        );
        assert_eq!(
            ApiFailure::classify(&server(404, " ")).message,
            "Not found."
        );
        assert_eq!(
            ApiFailure::classify(&status(401)).message,
            "Please sign in."
        );
        // Not ours: a proxy's page or a panic, decoded by the client's fallback.
        let raw = server(502, "HTTP 502: <html>Bad gateway</html>");
        assert_eq!(ApiFailure::classify(&raw).message, TRANSIENT_MESSAGE);
        let raw = ServerFnError::ServerError {
            message: "HTTP 409: plain text".to_owned(),
            code: 409,
            details: Some(serde_json::json!("x")),
        };
        let failure = ApiFailure::classify(&raw);
        assert_eq!(
            failure.message,
            "This conflicts with data that is already saved."
        );
        assert_eq!(failure.details, None);
    }

    #[test]
    fn details_and_retry_after_are_passed_on() {
        let error = ServerFnError::ServerError {
            message: "Invalid program.".to_owned(),
            code: 422,
            details: Some(serde_json::json!(["a"])),
        };
        let failure = ApiFailure::classify(&error);
        assert_eq!(failure.details, Some(serde_json::json!(["a"])));
        assert_eq!(failure.retry_after_secs(), None);
        let limited = ServerFnError::ServerError {
            message: "Too many requests.".to_owned(),
            code: 429,
            details: Some(serde_json::json!({ "retry_after_secs": 12 })),
        };
        assert_eq!(ApiFailure::classify(&limited).retry_after_secs(), Some(12));
        assert_eq!(
            ApiFailure::classify(&server(429, "x")).retry_after_secs(),
            None
        );
    }

    #[test]
    fn network_failures_are_retryable() {
        for error in [
            RequestError::Timeout("t".to_owned()),
            RequestError::Request("r".to_owned()),
            RequestError::Connect("c".to_owned()),
            RequestError::Body("b".to_owned()),
        ] {
            let failure = ApiFailure::from(&ServerFnError::Request(error));
            assert_eq!(failure.kind, FailureKind::Network);
            assert!(failure.is_retryable());
            assert_eq!(failure.message, NETWORK_MESSAGE);
        }
    }

    #[test]
    fn client_side_encoding_failures_are_not_retryable() {
        for error in [
            ServerFnError::Serialization("s".to_owned()),
            ServerFnError::Deserialization("d".to_owned()),
            ServerFnError::Request(RequestError::Decode("d".to_owned())),
            ServerFnError::Request(RequestError::Builder("b".to_owned())),
        ] {
            let failure = ApiFailure::classify(&error);
            assert_eq!(failure.kind, FailureKind::Other);
            assert!(!failure.is_retryable());
            assert_eq!(failure.message, GENERIC_MESSAGE);
        }
    }
}
