//! Sign-in (#5): passkeys and Sign in with Google, with server-side sessions.
//!
//! - [`types`]: what the server functions exchange with the client.
//! - [`api`]: the server functions.
//! - `browser`: the WebAuthn and Google popup calls in the browser (web build; other builds get
//!   a stub with the same API that reports "unsupported", so the UI compiles everywhere).
//! - [`webauthn_json`], [`google_popup`], [`browser_error`]: the target-independent logic behind
//!   `browser`, tested on the host.
//!
//! The server side lives in `crate::server::auth`. The design and threat model are in
//! `docs/auth.md`.

pub mod api;
#[cfg(feature = "web")]
pub mod browser;
// Outside the web build, only the stub's `Unsupported` is used (and the tests use the rest).
#[cfg_attr(not(feature = "web"), allow(dead_code))]
pub mod browser_error;
#[cfg(any(feature = "web", test))]
pub mod google_popup;
pub mod types;
#[cfg(any(feature = "web", test))]
pub mod webauthn_json;

// Declared after the others so rustfmt keeps this pair apart from the real `browser`.
#[cfg(not(feature = "web"))]
#[path = "auth/browser_stub.rs"]
#[allow(dead_code)]
pub mod browser;
