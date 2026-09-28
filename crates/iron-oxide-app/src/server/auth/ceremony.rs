//! One-time ceremony state: the WebAuthn challenge state, or Google's state, nonce and PKCE
//! verifier.
//!
//! The state itself is stored in `auth_ceremonies`; the session only holds the row's random id,
//! under one key per kind. Taking a ceremony removes the id from the session and deletes the row
//! in the same statement that reads it (`DELETE ... RETURNING`), so it can be used once, even by
//! two concurrent requests with the same cookie. Session data alone could not guarantee that:
//! each request works on its own copy, saved after the handler returns.

use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use sqlx::PgPool;
use time::OffsetDateTime;
use tower_sessions::Session;
use uuid::Uuid;

use super::{
    error::AuthError,
    session::{self, keys},
};
use crate::auth::types::UserId;

/// What a ceremony is for. Stored as `auth_ceremony_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "auth_ceremony_kind", rename_all = "snake_case")]
pub enum CeremonyKind {
    PasskeySignUp,
    PasskeySignIn,
    PasskeyAdd,
    GoogleSignIn,
    GoogleLink,
}

impl CeremonyKind {
    /// The session key holding this kind's ceremony id. Both Google kinds share one: the
    /// callback cannot tell them apart, and starting one replaces the other.
    #[must_use]
    pub const fn session_key(self) -> &'static str {
        match self {
            Self::PasskeySignUp => "auth.ceremony.passkey_sign_up",
            Self::PasskeySignIn => "auth.ceremony.passkey_sign_in",
            Self::PasskeyAdd => "auth.ceremony.passkey_add",
            Self::GoogleSignIn | Self::GoogleLink => "auth.ceremony.google",
        }
    }

    /// How long the user has to complete it.
    #[must_use]
    pub const fn ttl(self) -> Duration {
        match self {
            // The WebAuthn prompt times out after 5 minutes (webauthn-rs default).
            Self::PasskeySignUp | Self::PasskeySignIn | Self::PasskeyAdd => {
                Duration::from_secs(5 * 60)
            }
            // Google's consent screen can take longer (account chooser, 2FA).
            Self::GoogleSignIn | Self::GoogleLink => Duration::from_secs(10 * 60),
        }
    }

    /// Whether it is started by, and bound to, a signed-in user.
    #[must_use]
    pub const fn needs_user(self) -> bool {
        matches!(self, Self::PasskeyAdd | Self::GoogleLink)
    }
}

/// Stores a new ceremony of `kind` and points the session at it, replacing any earlier one of
/// the same kind (whose row then simply expires).
///
/// `user` must be `Some` exactly for the kinds that [`CeremonyKind::needs_user`].
pub async fn start<T: Serialize>(
    pool: &PgPool,
    session: &Session,
    kind: CeremonyKind,
    user: Option<UserId>,
    state: &T,
) -> Result<(), AuthError> {
    if kind.needs_user() != user.is_some() {
        return Err(AuthError::Internal(format!(
            "ceremony {kind:?} started with the wrong user binding"
        )));
    }
    let id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + kind.ttl();
    sqlx::query!(
        "INSERT INTO auth_ceremonies (id, kind, user_id, state, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        kind as CeremonyKind,
        user.map(|user| user.as_uuid()),
        serde_json::to_value(state)?,
        expires_at,
    )
    .execute(pool)
    .await?;

    session.insert(kind.session_key(), id).await?;
    // A signed-out visitor only needs the session for the ceremony: keep it short-lived.
    if session.get::<UserId>(keys::USER_ID).await?.is_none() {
        session.set_expiry(Some(session::anonymous_expiry()));
    }
    Ok(())
}

/// A ceremony taken out of the database.
pub struct Taken {
    pub kind: CeremonyKind,
    /// The user who started it (only for [`CeremonyKind::needs_user`] kinds).
    pub user: Option<UserId>,
    state: serde_json::Value,
}

impl Taken {
    /// The stored state.
    pub fn state<T: DeserializeOwned>(self) -> Result<T, AuthError> {
        Ok(serde_json::from_value(self.state)?)
    }
}

/// Takes (and deletes) the session's ceremony of `kind`.
///
/// Fails with [`AuthError::Ceremony`] if the session has none, it was already used, it expired,
/// or it is bound to a different user than `user` (see [`CeremonyKind::needs_user`]).
pub async fn take<T: DeserializeOwned>(
    pool: &PgPool,
    session: &Session,
    kind: CeremonyKind,
    user: Option<UserId>,
) -> Result<T, AuthError> {
    take_one_of(pool, session, &[kind], user).await?.state()
}

/// Takes (and deletes) the session's Google ceremony, whichever of sign-in or link it is.
/// `current` is the signed-in user, if any: a link ceremony must have been started by them.
pub async fn take_google(
    pool: &PgPool,
    session: &Session,
    current: Option<UserId>,
) -> Result<Taken, AuthError> {
    take_one_of(
        pool,
        session,
        &[CeremonyKind::GoogleSignIn, CeremonyKind::GoogleLink],
        current,
    )
    .await
}

/// `kinds` must share one session key.
async fn take_one_of(
    pool: &PgPool,
    session: &Session,
    kinds: &[CeremonyKind],
    current: Option<UserId>,
) -> Result<Taken, AuthError> {
    let Some(first) = kinds.first() else {
        return Err(AuthError::Internal("no ceremony kind".to_owned()));
    };
    let id = session
        .remove::<Uuid>(first.session_key())
        .await?
        .ok_or(AuthError::Ceremony("none in this session"))?;
    // Keep a signed-out session short-lived even if the ceremony fails.
    if session.get::<UserId>(keys::USER_ID).await?.is_none() {
        session.set_expiry(Some(session::anonymous_expiry()));
    }
    let row = sqlx::query!(
        r#"DELETE FROM auth_ceremonies WHERE id = $1 AND kind = ANY($2)
           RETURNING kind AS "kind: CeremonyKind", user_id, state, expires_at > now() AS "live!""#,
        id,
        kinds as &[CeremonyKind],
    )
    .fetch_optional(pool)
    .await?
    .ok_or(AuthError::Ceremony("unknown or already used"))?;

    if !row.live {
        return Err(AuthError::Ceremony("expired"));
    }
    let owner = row.user_id.map(UserId::from_uuid);
    let bound_ok = if row.kind.needs_user() {
        owner.is_some() && owner == current
    } else {
        owner.is_none()
    };
    if !bound_ok {
        return Err(AuthError::Ceremony("bound to another user"));
    }
    Ok(Taken {
        kind: row.kind,
        user: owner,
        state: row.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [CeremonyKind; 5] = [
        CeremonyKind::PasskeySignUp,
        CeremonyKind::PasskeySignIn,
        CeremonyKind::PasskeyAdd,
        CeremonyKind::GoogleSignIn,
        CeremonyKind::GoogleLink,
    ];

    #[test]
    fn session_keys_are_distinct_except_google() {
        let mut keys: Vec<&str> = ALL.iter().map(|kind| kind.session_key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ALL.len() - 1);
        assert!(keys.iter().all(|key| key.starts_with("auth.ceremony.")));
        assert_eq!(
            CeremonyKind::GoogleSignIn.session_key(),
            CeremonyKind::GoogleLink.session_key()
        );
    }

    #[test]
    fn ceremonies_are_short_lived() {
        for kind in ALL {
            assert!(kind.ttl() <= Duration::from_secs(10 * 60), "{kind:?}");
            assert!(kind.ttl() <= session::ANONYMOUS_TIMEOUT, "{kind:?}");
        }
    }

    #[test]
    fn only_add_and_link_are_bound_to_a_user() {
        let bound: Vec<CeremonyKind> = ALL.into_iter().filter(|k| k.needs_user()).collect();
        assert_eq!(bound, [CeremonyKind::PasskeyAdd, CeremonyKind::GoogleLink]);
    }
}
