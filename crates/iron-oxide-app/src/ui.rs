//! User interface components.

mod account;

use dioxus::CapturedError;
use dioxus::fullstack::RequestError;
use dioxus::prelude::*;

use crate::api::time::server_time;
use crate::pwa::PwaHead;

/// Colour tokens shared by every screen (see docs/palette.md).
const TOKENS_CSS: Asset = asset!("/assets/tokens.css");

/// Styles of the app shell and the account panel.
const AUTH_CSS: Asset = asset!("/assets/auth.css");

/// Root component: the account panel, and one server function round-trip.
#[component]
pub fn App() -> Element {
    let mut time = use_action(server_time);

    rsx! {
        document::Title { "Iron Oxide" }
        PwaHead {}
        document::Meta { name: "viewport", content: "width=device-width, initial-scale=1" }
        document::Stylesheet { href: TOKENS_CSS }
        document::Stylesheet { href: AUTH_CSS }
        main { class: "io-app",
            header { class: "io-header",
                h1 { "Iron Oxide" }
                p { class: "io-muted", "Zero-cost gains. The only overhead is the barbell." }
            }
            account::Account {}
            section { class: "io-card io-demo",
            button {
                id: "server-time",
                class: "io-button io-button-secondary",
                disabled: time.pending(),
                onclick: move |_| {
                    time.call();
                },
                "Ask the server for the time"
            }
            match time.value() {
                None if time.pending() => rsx! { p { "Asking the server…" } },
                None => rsx! {},
                Some(Ok(seconds)) => rsx! { p { id: "server-time-result", "Server time: {seconds} seconds since the Unix epoch" } },
                Some(Err(error)) => rsx! { p { id: "server-time-error", role: "alert", "{CallFailure::from_error(&error)}" } },
            }
            }
        }
    }
}

/// Why a server function call failed, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CallFailure {
    /// The request never got an HTTP response: offline, server down, timeout.
    Unreachable,
    /// The server answered with an error.
    ServerError { status: u16, message: String },
    /// The server answered, but the response could not be understood, or the request could not be
    /// built on the client.
    Unexpected(String),
}

impl CallFailure {
    fn from_error(error: &CapturedError) -> Self {
        match error.downcast_ref::<ServerFnError>() {
            Some(error) => Self::from_server_fn_error(error),
            None => Self::Unexpected(error.to_string()),
        }
    }

    fn from_server_fn_error(error: &ServerFnError) -> Self {
        match error {
            ServerFnError::ServerError { message, code, .. } => Self::ServerError {
                status: *code,
                message: message.clone(),
            },
            ServerFnError::Request(RequestError::Status(message, code)) => Self::ServerError {
                status: *code,
                message: message.clone(),
            },
            ServerFnError::Request(
                RequestError::Request(_)
                | RequestError::Connect(_)
                | RequestError::Timeout(_)
                | RequestError::Redirect(_),
            ) => Self::Unreachable,
            other => Self::Unexpected(other.to_string()),
        }
    }
}

impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable => {
                write!(
                    f,
                    "Could not reach the server. Check your connection and try again."
                )
            }
            Self::ServerError { status, message } => {
                write!(f, "The server returned an error ({status}): {message}")
            }
            Self::Unexpected(details) => {
                write!(f, "Unexpected response from the server: {details}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(error: ServerFnError) -> CallFailure {
        CallFailure::from_error(&CapturedError::from(error))
    }

    #[test]
    fn server_error_is_a_server_error_with_its_status() {
        let failure = classify(ServerFnError::ServerError {
            message: "boom".into(),
            code: 503,
            details: None,
        });
        assert_eq!(
            failure,
            CallFailure::ServerError {
                status: 503,
                message: "boom".into()
            }
        );
        assert_eq!(
            failure.to_string(),
            "The server returned an error (503): boom"
        );
    }

    #[test]
    fn http_status_error_is_a_server_error() {
        let failure = classify(ServerFnError::Request(RequestError::Status(
            "Bad Gateway".into(),
            502,
        )));
        assert_eq!(
            failure,
            CallFailure::ServerError {
                status: 502,
                message: "Bad Gateway".into()
            }
        );
    }

    #[test]
    fn network_failures_are_unreachable() {
        for error in [
            RequestError::Request("connection refused".into()),
            RequestError::Connect("reset".into()),
            RequestError::Timeout("30s".into()),
            RequestError::Redirect("loop".into()),
        ] {
            assert_eq!(
                classify(ServerFnError::Request(error)),
                CallFailure::Unreachable
            );
        }
        assert!(
            CallFailure::Unreachable
                .to_string()
                .starts_with("Could not reach the server")
        );
    }

    #[test]
    fn undecodable_response_is_unexpected() {
        for error in [
            ServerFnError::Deserialization("bad json".into()),
            ServerFnError::Request(RequestError::Decode("bad body".into())),
            ServerFnError::Request(RequestError::Body("truncated".into())),
        ] {
            let failure = classify(error);
            assert!(matches!(failure, CallFailure::Unexpected(_)), "{failure:?}");
            assert!(
                failure
                    .to_string()
                    .starts_with("Unexpected response from the server: ")
            );
        }
    }

    #[test]
    fn non_server_fn_error_is_unexpected() {
        let error = CapturedError::from(std::fmt::Error);
        assert!(matches!(
            CallFailure::from_error(&error),
            CallFailure::Unexpected(_)
        ));
    }
}
