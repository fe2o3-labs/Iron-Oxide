//! Sign-in errors, and how they reach the client.
//!
//! The client only ever sees a status code and a short generic message. Details (which check
//! failed, database errors) are logged server-side, never returned, so a failing ceremony does not
//! tell an attacker which part to change.

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;

/// Why a sign-in operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No valid session: signed out, expired, or the user no longer exists.
    #[error("not signed in")]
    Unauthenticated,
    /// The ceremony (challenge, state) is missing, expired, already used, or belongs to another
    /// user or session.
    #[error("sign-in ceremony not found, expired or already used ({0})")]
    Ceremony(&'static str),
    /// WebAuthn verification failed.
    #[error("passkey verification failed: {0}")]
    WebAuthn(#[source] webauthn_rs::prelude::WebauthnError),
    /// The credential verified, but does not match a passkey we know for that user.
    #[error("unknown passkey")]
    UnknownPasskey,
    /// The credential is already registered (to this or another account).
    #[error("this passkey is already registered")]
    PasskeyAlreadyRegistered,
    /// The authenticator reported that it did not store a discoverable credential, which could
    /// never be used for username-less sign-in.
    #[error("the authenticator did not create a discoverable credential")]
    NotDiscoverable,
    /// The Google identity is already linked to a different account.
    #[error("this Google account is linked to another account")]
    GoogleLinkedElsewhere,
    /// The signed-in user already has a (different) Google account linked.
    #[error("a different Google account is already linked")]
    GoogleAlreadyLinked,
    /// Google (or the ID token it returned) failed a check.
    #[error("Google sign-in failed: {0}")]
    Google(String),
    /// Removing this would leave the account with no way to sign in.
    #[error("cannot remove the last way to sign in")]
    LastSignInMethod,
    /// The target (passkey, identity) does not exist for this user.
    #[error("not found")]
    NotFound,
    /// Invalid user input, with a message that is safe to show.
    #[error("{0}")]
    Invalid(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("session store error: {0}")]
    Session(#[from] tower_sessions::session::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl AuthError {
    /// The HTTP status and the message shown to the user.
    #[must_use]
    pub fn public(&self) -> (u16, &str) {
        match self {
            Self::Unauthenticated => (401, "Please sign in."),
            Self::Ceremony(_) | Self::WebAuthn(_) | Self::UnknownPasskey | Self::Google(_) => {
                (400, "Sign-in failed. Please try again.")
            }
            Self::NotDiscoverable => (
                400,
                "This authenticator cannot store a passkey for username-less sign-in. Try your \
                 phone or computer's built-in passkeys.",
            ),
            Self::PasskeyAlreadyRegistered => (409, "This passkey is already registered."),
            Self::GoogleLinkedElsewhere => (
                409,
                "This Google account is already used by another account.",
            ),
            Self::GoogleAlreadyLinked => (
                409,
                "A different Google account is already linked to your account.",
            ),
            Self::LastSignInMethod => (
                409,
                "This is your last way to sign in. Add another passkey or link Google first.",
            ),
            Self::NotFound => (404, "Not found."),
            Self::Invalid(message) => (400, message),
            Self::Database(_) | Self::Session(_) | Self::Internal(_) => {
                (500, "Something went wrong. Please try again.")
            }
        }
    }

    /// Logs the details at a level matching the cause.
    fn log(&self) {
        match self {
            Self::Database(_) | Self::Session(_) | Self::Internal(_) => {
                tracing::error!(error = %self, "sign-in error");
            }
            Self::Unauthenticated => tracing::debug!(error = %self, "sign-in error"),
            _ => tracing::warn!(error = %self, "sign-in rejected"),
        }
    }
}

impl From<AuthError> for ServerFnError {
    fn from(error: AuthError) -> Self {
        error.log();
        let (code, message) = error.public();
        ServerFnError::ServerError {
            message: message.to_owned(),
            code,
            details: None,
        }
    }
}

impl From<webauthn_rs::prelude::WebauthnError> for AuthError {
    fn from(error: webauthn_rs::prelude::WebauthnError) -> Self {
        Self::WebAuthn(error)
    }
}

impl From<serde_json::Error> for AuthError {
    fn from(error: serde_json::Error) -> Self {
        Self::Internal(format!("JSON: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthenticated_is_401() {
        let error = ServerFnError::from(AuthError::Unauthenticated);
        assert!(matches!(
            error,
            ServerFnError::ServerError { code: 401, .. }
        ));
    }

    #[test]
    fn internal_details_never_reach_the_client() {
        let secret = "relation \"users\" does not exist at 10.0.0.3";
        for error in [
            AuthError::Internal(secret.to_owned()),
            AuthError::Database(sqlx::Error::Protocol(secret.to_owned())),
            AuthError::Google(secret.to_owned()),
            AuthError::Ceremony("state mismatch"),
        ] {
            let ServerFnError::ServerError { message, code, .. } = ServerFnError::from(error)
            else {
                panic!("not a server error");
            };
            assert!(!message.contains("10.0.0.3"), "{message}");
            assert!(!message.contains("mismatch"), "{message}");
            assert!(code == 400 || code == 500, "{code}");
        }
    }

    #[test]
    fn conflicts_are_409() {
        for error in [
            AuthError::PasskeyAlreadyRegistered,
            AuthError::GoogleLinkedElsewhere,
            AuthError::GoogleAlreadyLinked,
            AuthError::LastSignInMethod,
        ] {
            assert_eq!(error.public().0, 409, "{error}");
        }
    }

    #[test]
    fn invalid_input_message_is_shown() {
        let error = AuthError::Invalid("Nickname must be at most 64 characters".to_owned());
        assert_eq!(
            error.public(),
            (400, "Nickname must be at most 64 characters")
        );
    }
}
