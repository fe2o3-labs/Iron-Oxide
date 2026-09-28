//! Errors from the browser side of sign-in (WebAuthn calls, the Google popup), with messages fit
//! for the user. Target-independent so the mapping is tested on the host.

use std::fmt;

/// Which WebAuthn call failed: some `DOMException`s mean different things for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceremony {
    /// `navigator.credentials.create()`.
    Create,
    /// `navigator.credentials.get()`.
    Get,
}

/// Why a browser-side sign-in step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserError {
    /// The user cancelled, or the request timed out (`NotAllowedError`, `AbortError`).
    Cancelled,
    /// `create()` found one of the excluded credentials on this authenticator
    /// (`InvalidStateError`).
    AlreadyRegistered,
    /// No WebAuthn in this browser (no `PublicKeyCredential`, or `NotSupportedError`).
    Unsupported,
    /// The page's origin cannot use the relying party id (`SecurityError`).
    Security,
    /// Any other rejection, by `DOMException` or error name.
    Failed { name: String },
    /// The browser or the server handed back data we could not convert.
    Malformed(String),
}

impl BrowserError {
    /// Maps a rejected `create()`/`get()` promise by its error `name`.
    #[must_use]
    pub fn from_exception_name(ceremony: Ceremony, name: &str) -> Self {
        match (ceremony, name) {
            (_, "NotAllowedError" | "AbortError") => Self::Cancelled,
            (Ceremony::Create, "InvalidStateError") => Self::AlreadyRegistered,
            (_, "NotSupportedError") => Self::Unsupported,
            (_, "SecurityError") => Self::Security,
            (_, name) => Self::Failed {
                // Names are short identifiers; cap anything odd before it reaches the page.
                name: name.chars().take(64).collect(),
            },
        }
    }

    /// Whether the user backed out; shown as a gentle note rather than an error.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for BrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Passkey request cancelled. You can try again."),
            Self::AlreadyRegistered => f.write_str("This passkey is already registered."),
            Self::Unsupported => f.write_str("Passkeys are not supported on this browser."),
            Self::Security => f.write_str("This browser refused to use passkeys on this address."),
            Self::Failed { name } if name.is_empty() => {
                f.write_str("The passkey request failed. Please try again.")
            }
            Self::Failed { name } => {
                write!(f, "The passkey request failed ({name}). Please try again.")
            }
            Self::Malformed(_) => {
                f.write_str("The passkey request returned unexpected data. Please try again.")
            }
        }
    }
}

impl std::error::Error for BrowserError {}

#[cfg(any(feature = "web", test))]
impl From<super::webauthn_json::ConversionError> for BrowserError {
    fn from(error: super::webauthn_json::ConversionError) -> Self {
        Self::Malformed(error.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_allowed_and_abort_are_cancellations() {
        for ceremony in [Ceremony::Create, Ceremony::Get] {
            for name in ["NotAllowedError", "AbortError"] {
                let error = BrowserError::from_exception_name(ceremony, name);
                assert_eq!(error, BrowserError::Cancelled);
                assert!(error.is_cancelled());
            }
        }
    }

    #[test]
    fn invalid_state_means_already_registered_only_on_create() {
        assert_eq!(
            BrowserError::from_exception_name(Ceremony::Create, "InvalidStateError"),
            BrowserError::AlreadyRegistered
        );
        assert_eq!(
            BrowserError::from_exception_name(Ceremony::Get, "InvalidStateError"),
            BrowserError::Failed {
                name: "InvalidStateError".to_owned()
            }
        );
    }

    #[test]
    fn not_supported_and_security_have_their_own_variants() {
        assert_eq!(
            BrowserError::from_exception_name(Ceremony::Get, "NotSupportedError"),
            BrowserError::Unsupported
        );
        assert_eq!(
            BrowserError::from_exception_name(Ceremony::Create, "SecurityError"),
            BrowserError::Security
        );
    }

    #[test]
    fn other_names_are_kept_and_capped() {
        let error = BrowserError::from_exception_name(Ceremony::Get, "TypeError");
        assert_eq!(
            error.to_string(),
            "The passkey request failed (TypeError). Please try again."
        );
        assert!(!error.is_cancelled());
        let long = "X".repeat(500);
        let BrowserError::Failed { name } = BrowserError::from_exception_name(Ceremony::Get, &long)
        else {
            panic!("expected Failed");
        };
        assert_eq!(name.len(), 64);
        assert_eq!(
            BrowserError::Failed {
                name: String::new()
            }
            .to_string(),
            "The passkey request failed. Please try again."
        );
    }

    #[test]
    fn messages_are_user_facing() {
        assert!(BrowserError::Cancelled.to_string().contains("cancelled"));
        assert_eq!(
            BrowserError::AlreadyRegistered.to_string(),
            "This passkey is already registered."
        );
        assert_eq!(
            BrowserError::Unsupported.to_string(),
            "Passkeys are not supported on this browser."
        );
        assert!(!BrowserError::Security.to_string().is_empty());
        // Internal details stay out of the message.
        let malformed = BrowserError::Malformed("rawId was a number".to_owned());
        assert!(!malformed.to_string().contains("rawId"));
    }

    #[test]
    fn conversion_errors_are_malformed() {
        let error =
            BrowserError::from(super::super::webauthn_json::ConversionError("x".to_owned()));
        assert_eq!(error, BrowserError::Malformed("x".to_owned()));
    }
}
