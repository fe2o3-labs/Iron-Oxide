//! Stand-in for `browser` in builds without the `web` feature (the server and host tests).
//!
//! Same API, so the UI compiles everywhere; nothing here runs in practice, since browser calls
//! only happen in event handlers and client-side effects.

use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

pub use super::browser_error::BrowserError;
use super::types::GoogleCallbackMessage;

/// Always unsupported outside the browser.
pub async fn create_passkey(
    _options: CreationChallengeResponse,
) -> Result<RegisterPublicKeyCredential, BrowserError> {
    Err(BrowserError::Unsupported)
}

/// Always unsupported outside the browser.
pub async fn get_passkey(
    _options: RequestChallengeResponse,
) -> Result<PublicKeyCredential, BrowserError> {
    Err(BrowserError::Unsupported)
}

/// Never returns outside the browser (nothing polls there).
pub async fn sleep(_ms: i32) {
    std::future::pending::<()>().await;
}

/// Never confirmed outside the browser.
#[must_use]
pub fn confirm(_message: &str) -> bool {
    false
}

/// No popup outside the browser.
pub struct GooglePopup;

/// Where the Google page was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleNavigation {
    /// In the popup.
    Popup,
    /// In this window.
    Redirect,
}

impl GooglePopup {
    /// No popup outside the browser.
    #[must_use]
    pub fn open() -> Self {
        Self
    }

    /// Never open outside the browser.
    #[must_use]
    pub fn is_open(&self) -> bool {
        false
    }

    /// Always unsupported outside the browser.
    pub fn navigate(&self, _url: &str) -> Result<GoogleNavigation, BrowserError> {
        Err(BrowserError::Unsupported)
    }

    /// Nothing to close.
    pub fn close(&self) {}
}

/// Listens for nothing outside the browser.
pub struct GoogleCallbackListener;

impl GoogleCallbackListener {
    /// Never calls `on_message` outside the browser.
    pub fn install(_on_message: impl Fn(GoogleCallbackMessage) + 'static) -> Self {
        Self
    }
}
