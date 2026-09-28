//! Types shared by the sign-in server functions and the client.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies a user account (`users.id`).
///
/// Only the server creates one from the session; a user id sent by the client is never trusted.
// TODO(#48): replace with `iron_oxide_domain::UserId` once the domain's typed IDs land; the API
// (`from_uuid` / `as_uuid`, transparent serde) is the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(Uuid);

impl UserId {
    /// Wraps an existing UUID (e.g. one read from the database).
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Identifies one of a user's passkeys (`passkeys.id`, not the WebAuthn credential ID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PasskeyId(Uuid);

impl PasskeyId {
    /// Wraps an existing UUID.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for PasskeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// The signed-in user, as shown by the account screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user_id: UserId,
    /// The name given at sign-up, if any.
    pub display_name: Option<String>,
    /// The user's passkeys, oldest first.
    pub passkeys: Vec<PasskeyInfo>,
    /// Whether a Google account is linked.
    pub google_linked: bool,
}

impl Me {
    /// How many ways the user has to sign in. Removing the last one is refused.
    #[must_use]
    pub fn sign_in_methods(&self) -> usize {
        self.passkeys.len() + usize::from(self.google_linked)
    }
}

/// One passkey, without any key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasskeyInfo {
    pub id: PasskeyId,
    pub nickname: String,
    /// RFC 3339 (UTC).
    pub created_at: String,
    /// RFC 3339 (UTC); `None` if never used to sign in.
    pub last_used_at: Option<String>,
    /// Whether the passkey is synced by its provider (iCloud Keychain, Google Password Manager…).
    pub backed_up: bool,
}

/// What a Google sign-in ceremony is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoogleIntent {
    /// Sign in (or create an account) with Google.
    SignIn,
    /// Link a Google account to the signed-in user.
    Link,
}

/// Longest accepted display name or passkey nickname, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// Trims a user-supplied display name or nickname and checks its length.
///
/// Returns `Ok(None)` for a blank name, and an error message for one that is too long or contains
/// control characters.
pub fn normalize_name(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(format!("must be at most {MAX_NAME_CHARS} characters"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("must not contain control characters".to_owned());
    }
    Ok(Some(trimmed.to_owned()))
}

/// The message the Google callback page posts to the window that opened it. Serialized as JSON.
///
/// The opener only accepts it from our own origin (`MessageEvent.origin`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoogleCallbackMessage {
    /// The callback finished the sign-in itself (the popup shares the app's cookies): reload the
    /// signed-in user.
    Done,
    /// The popup could not finish (it does not share the app's session): the opener must call
    /// `google_finish(code, state)` itself. The code is useless without the PKCE verifier, which
    /// never leaves the server.
    Code { code: String, state: String },
    /// Google or the server refused; `message` is safe to show.
    Error { message: String },
}

/// The `BroadcastChannel` name the callback page also posts its message on, for when the popup
/// has no `window.opener` (same-origin only by definition).
pub const GOOGLE_CALLBACK_CHANNEL: &str = "iron-oxide-google-sign-in";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_name_trims_and_accepts_normal_names() {
        assert_eq!(
            normalize_name("  Jules  ").unwrap(),
            Some("Jules".to_owned())
        );
        assert_eq!(
            normalize_name("Élodie 💪").unwrap(),
            Some("Élodie 💪".to_owned())
        );
    }

    #[test]
    fn normalize_name_treats_blank_as_none() {
        assert_eq!(normalize_name("").unwrap(), None);
        assert_eq!(normalize_name(" \t ").unwrap(), None);
    }

    #[test]
    fn normalize_name_limits_length_in_characters() {
        let max = "é".repeat(MAX_NAME_CHARS);
        assert_eq!(normalize_name(&max).unwrap(), Some(max.clone()));
        assert!(normalize_name(&format!("{max}é")).is_err());
    }

    #[test]
    fn normalize_name_rejects_control_characters() {
        assert!(normalize_name("a\u{0}b").is_err());
        assert!(normalize_name("a\nb").is_err());
    }

    #[test]
    fn sign_in_methods_counts_passkeys_and_google() {
        let passkey = PasskeyInfo {
            id: PasskeyId::from_uuid(Uuid::nil()),
            nickname: "Phone".to_owned(),
            created_at: "2026-09-28T00:00:00Z".to_owned(),
            last_used_at: None,
            backed_up: true,
        };
        let mut me = Me {
            user_id: UserId::from_uuid(Uuid::nil()),
            display_name: None,
            passkeys: vec![],
            google_linked: false,
        };
        assert_eq!(me.sign_in_methods(), 0);
        me.google_linked = true;
        assert_eq!(me.sign_in_methods(), 1);
        me.passkeys = vec![passkey.clone(), passkey];
        assert_eq!(me.sign_in_methods(), 3);
    }

    #[test]
    fn callback_messages_have_a_stable_json_shape() {
        let json = serde_json_like(&GoogleCallbackMessage::Code {
            code: "c".to_owned(),
            state: "s".to_owned(),
        });
        assert_eq!(json, r#"{"type":"code","code":"c","state":"s"}"#);
        assert_eq!(
            serde_json_like(&GoogleCallbackMessage::Done),
            r#"{"type":"done"}"#
        );
    }

    fn serde_json_like<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }
}
