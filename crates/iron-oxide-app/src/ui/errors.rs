//! Error surfacing: every failed server call ends up in front of the user, never swallowed (#26).
//!
//! A screen that calls a server function hands any error to [`use_errors`]'s
//! [`Errors::report`]. It is classified with [`ApiFailure::classify`] (`docs/api.md`), turned into
//! a message by [`surface`], and shown in the banner at the top of every page
//! ([`crate::ui::components::BannerHost`]). A `401` also signs the app out, so the shell shows the
//! sign-in screen.

use dioxus::CapturedError;
use dioxus::prelude::*;

use crate::api::error::{ApiFailure, FailureKind, GENERIC_MESSAGE};
use crate::ui::shell::{SessionStatus, use_session};

/// How a banner looks and is announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// Something failed (`role="alert"`).
    Error,
    /// A note (`role="status"`).
    Info,
    /// Something degraded that does not block the user, such as being offline (`role="status"`).
    Warning,
}

/// The message at the top of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub kind: BannerKind,
    pub message: String,
    /// Increases with every banner, so that the same message reported twice is shown again.
    pub id: u64,
}

/// What the UI does about a failed call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surfaced {
    /// The message for the banner.
    pub message: String,
    /// Whether the session is gone and the sign-in screen must be shown (`401`).
    pub sign_in: bool,
}

/// Decides what the user sees for `failure`. The message is the classified one (the server's own
/// for 4xx and 503, a generic one otherwise). Our rate limiter's `429` message already says how
/// long to wait; only a `429` whose message does not gets the wait from its details added.
#[must_use]
pub fn surface(failure: &ApiFailure) -> Surfaced {
    let says_when = failure
        .message
        .to_ascii_lowercase()
        .contains("try again in");
    let message = match failure.retry_after_secs() {
        Some(secs) if !says_when => {
            format!("{} Try again in {}.", failure.message, wait_text(secs))
        }
        _ => failure.message.clone(),
    };
    Surfaced {
        message,
        sign_in: failure.kind == FailureKind::Unauthorized,
    }
}

/// `12 s`, `1 min`, `2 min 5 s`: a short wait, readable at a glance.
#[must_use]
pub fn wait_text(secs: u64) -> String {
    match (secs / 60, secs % 60) {
        (0, seconds) => format!("{} s", seconds.max(1)),
        (minutes, 0) => format!("{minutes} min"),
        (minutes, seconds) => format!("{minutes} min {seconds} s"),
    }
}

/// The banner shared by every page, provided by the app root.
#[derive(Clone, Copy, PartialEq)]
pub struct Errors {
    banner: Signal<Option<Banner>>,
    next_id: Signal<u64>,
    session: Signal<SessionStatus>,
}

impl Errors {
    /// Shows `error` in the banner; a `401` also shows the sign-in screen.
    pub fn report(self, error: &ServerFnError) {
        let surfaced = surface(&ApiFailure::classify(error));
        if surfaced.sign_in {
            let mut session = self.session;
            session.set(SessionStatus::SignedOut);
        }
        self.show(BannerKind::Error, surfaced.message);
    }

    /// Same as [`Errors::report`], for the errors of `use_action` and `use_resource`.
    pub fn report_captured(self, error: &CapturedError) {
        match error.downcast_ref::<ServerFnError>() {
            Some(error) => self.report(error),
            None => {
                self.show(BannerKind::Error, GENERIC_MESSAGE);
            }
        }
    }

    /// Shows a banner, replacing the current one. Returns its id, for [`Errors::dismiss_if`].
    pub fn show(mut self, kind: BannerKind, message: impl Into<String>) -> u64 {
        let id = *self.next_id.peek();
        self.next_id.set(id + 1);
        self.banner.set(Some(Banner {
            kind,
            message: message.into(),
            id,
        }));
        id
    }

    /// Hides the banner.
    pub fn dismiss(mut self) {
        self.banner.set(None);
    }

    /// Hides the banner if it is still the one with this `id` (not replaced by a newer one).
    pub fn dismiss_if(mut self, id: u64) {
        let current = self.banner.peek().as_ref().map(|banner| banner.id);
        if current == Some(id) {
            self.banner.set(None);
        }
    }

    /// The banner on screen, if any.
    #[must_use]
    pub fn banner(&self) -> Option<Banner> {
        self.banner.read().clone()
    }
}

/// Provides the shared banner. Called once, by the app root, after the session is provided.
pub fn use_errors_provider() -> Errors {
    let session = use_session();
    let banner = use_signal(|| None);
    let next_id = use_signal(|| 0);
    use_context_provider(|| Errors {
        banner,
        next_id,
        session,
    })
}

/// The shared banner, to report errors to.
#[must_use]
pub fn use_errors() -> Errors {
    use_context::<Errors>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::fullstack::RequestError;

    fn surfaced(error: &ServerFnError) -> Surfaced {
        surface(&ApiFailure::classify(error))
    }

    fn server(code: u16, message: &str, details: Option<serde_json::Value>) -> ServerFnError {
        ServerFnError::ServerError {
            message: message.to_owned(),
            code,
            details,
        }
    }

    #[test]
    fn a_401_asks_for_sign_in() {
        let shown = surfaced(&server(401, "Please sign in.", None));
        assert!(shown.sign_in);
        assert_eq!(shown.message, "Please sign in.");
        let bare = surfaced(&ServerFnError::Request(RequestError::Status(
            "Unauthorized".to_owned(),
            401,
        )));
        assert!(bare.sign_in);
    }

    #[test]
    fn other_failures_keep_the_session() {
        for code in [403, 404, 409, 422, 429, 500, 503] {
            assert!(!surfaced(&server(code, "m", None)).sign_in, "{code}");
        }
        let offline = surfaced(&ServerFnError::Request(RequestError::Connect("x".into())));
        assert!(!offline.sign_in);
        assert_eq!(offline.message, crate::api::error::NETWORK_MESSAGE);
    }

    #[test]
    fn server_messages_are_shown_for_4xx() {
        assert_eq!(
            surfaced(&server(403, "Custom programs need the Pro plan.", None)).message,
            "Custom programs need the Pro plan."
        );
        assert_eq!(
            surfaced(&server(422, "Reps must be at least 1.", None)).message,
            "Reps must be at least 1."
        );
        assert_eq!(
            surfaced(&server(500, "panic at db.rs:12", None)).message,
            crate::api::error::GENERIC_MESSAGE
        );
    }

    /// The `429` our rate limiter sends: its message already has the wait, and is shown as it is.
    #[cfg(feature = "server")]
    #[test]
    fn the_rate_limiter_429_is_shown_as_it_is() {
        for secs in [1, 42, 125] {
            let message = crate::server::rate_limit::too_many_requests_message(secs);
            let details = serde_json::json!({ "retry_after_secs": secs });
            let shown = surfaced(&server(429, &message, Some(details)));
            assert_eq!(shown.message, message);
            assert_eq!(shown.message.matches("again in").count(), 1);
            assert!(!shown.sign_in);
        }
    }

    /// The same message, as the gallery shows it, without the server feature.
    #[test]
    fn a_429_that_says_when_is_not_repeated() {
        let message = "Too many requests. Please try again in 1 second.";
        let details = serde_json::json!({ "retry_after_secs": 1 });
        assert_eq!(
            surfaced(&server(429, message, Some(details))).message,
            message
        );
    }

    #[test]
    fn a_429_that_does_not_say_when_gets_the_wait() {
        let details = serde_json::json!({ "retry_after_secs": 12 });
        assert_eq!(
            surfaced(&server(429, "Too many requests.", Some(details))).message,
            "Too many requests. Try again in 12 s."
        );
        let details = serde_json::json!({ "retry_after_secs": 125 });
        assert_eq!(
            surfaced(&server(429, "Too many requests.", Some(details))).message,
            "Too many requests. Try again in 2 min 5 s."
        );
        // Without the delay, the message alone.
        assert_eq!(
            surfaced(&server(429, "Too many requests.", None)).message,
            "Too many requests."
        );
    }

    #[test]
    fn waits_read_at_a_glance() {
        assert_eq!(wait_text(0), "1 s");
        assert_eq!(wait_text(1), "1 s");
        assert_eq!(wait_text(59), "59 s");
        assert_eq!(wait_text(60), "1 min");
        assert_eq!(wait_text(61), "1 min 1 s");
        assert_eq!(wait_text(600), "10 min");
    }
}
