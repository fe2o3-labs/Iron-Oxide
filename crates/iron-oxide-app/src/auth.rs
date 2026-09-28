//! Sign-in (#5): passkeys and Sign in with Google, with server-side sessions.
//!
//! - [`types`]: what the server functions exchange with the client.
//! - [`api`]: the server functions.
//! - `browser` (web build only): the WebAuthn and Google popup calls in the browser.
//!
//! The server side lives in `crate::server::auth`. The design and threat model are in
//! `docs/auth.md`.

pub mod api;
#[cfg(feature = "web")]
pub mod browser;
pub mod types;
