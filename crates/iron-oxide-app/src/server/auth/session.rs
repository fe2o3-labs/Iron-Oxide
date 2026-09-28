//! Server-side sessions: `tower-sessions` with our own Postgres store.
//!
//! Why our own store rather than `tower-sessions-sqlx-store`: that crate still implements the
//! previous `tower-sessions-core` (0.14) and creates its own table without a user column. Ours
//! lives in a normal migration, keeps `user_id` (so deleting a user deletes their sessions), and
//! stores only a hash of the session ID.
//!
//! Lifetimes:
//! - a signed-in session expires after [`IDLE_TIMEOUT`] without activity, and in any case
//!   [`ABSOLUTE_TIMEOUT`] after sign-in (checked by [`super::AuthContext::current_user`]);
//! - a signed-out session only exists while a ceremony is in flight, for [`ANONYMOUS_TIMEOUT`].

use std::time::Duration;

use async_trait::async_trait;
use dioxus::logger::tracing;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use tower_sessions::{
    Expiry, SessionManagerLayer, SessionStore,
    cookie::{Key, SameSite},
    service::SignedCookie,
    session::{Id, Record},
    session_store,
};
use uuid::Uuid;

use crate::auth::types::UserId;

/// A signed-in session ends after this long without a request that refreshes it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// A signed-in session ends this long after sign-in, whatever the activity.
pub const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The idle expiry is pushed back at most this often (each push is a database write).
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// A signed-out session (holding only an in-flight ceremony) lives this long.
pub const ANONYMOUS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often expired sessions and ceremonies are deleted. Rare on purpose: each run wakes the
/// scale-to-zero Neon compute (#41).
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Session keys. Only the server reads or writes them.
pub mod keys {
    /// The signed-in [`UserId`](crate::auth::types::UserId).
    pub const USER_ID: &str = "auth.user_id";
    /// Unix seconds of the sign-in, for the absolute timeout.
    pub const SIGNED_IN_AT: &str = "auth.signed_in_at";
    /// Unix seconds of the last idle-expiry refresh.
    pub const LAST_SEEN_AT: &str = "auth.last_seen_at";
}

/// Cookie name when `Secure`: the `__Host-` prefix makes browsers refuse it unless it is
/// `Secure`, has `Path=/` and no `Domain`, so a sibling subdomain cannot plant or overwrite it.
pub const SECURE_COOKIE_NAME: &str = "__Host-iron_oxide_session";
/// Cookie name for local development over plain http (the prefix requires `Secure`).
pub const DEV_COOKIE_NAME: &str = "iron_oxide_session";

/// The session cookie name for this deployment.
#[must_use]
pub fn cookie_name(secure: bool) -> &'static str {
    if secure {
        SECURE_COOKIE_NAME
    } else {
        DEV_COOKIE_NAME
    }
}

/// Converts a std duration into the `time` duration `tower-sessions` uses.
fn time_duration(duration: Duration) -> time::Duration {
    time::Duration::try_from(duration).unwrap_or(time::Duration::MAX)
}

/// The expiry of a signed-in session, refreshed on activity.
#[must_use]
pub fn signed_in_expiry() -> Expiry {
    Expiry::OnInactivity(time_duration(IDLE_TIMEOUT))
}

/// The expiry of a signed-out session holding a ceremony.
#[must_use]
pub fn anonymous_expiry() -> Expiry {
    Expiry::OnInactivity(time_duration(ANONYMOUS_TIMEOUT))
}

/// Builds the session layer: `HttpOnly; SameSite=Lax; Path=/`, `Secure` unless local http, the
/// cookie value signed with `key` (so a forged or truncated ID is rejected before any lookup).
pub fn layer(
    store: PgSessionStore,
    key: Key,
    secure: bool,
) -> SessionManagerLayer<PgSessionStore, SignedCookie> {
    SessionManagerLayer::new(store)
        .with_name(cookie_name(secure))
        .with_http_only(true)
        .with_secure(secure)
        .with_same_site(SameSite::Lax)
        .with_path("/")
        .with_expiry(signed_in_expiry())
        .with_signed(key)
}

/// Stores sessions in the `sessions` table.
#[derive(Debug, Clone)]
pub struct PgSessionStore {
    pool: PgPool,
}

impl PgSessionStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Deletes expired sessions and ceremonies. Returns how many rows went.
    pub async fn delete_expired(&self) -> Result<u64, sqlx::Error> {
        let sessions = sqlx::query!("DELETE FROM sessions WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?
            .rows_affected();
        let ceremonies = sqlx::query!("DELETE FROM auth_ceremonies WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(sessions + ceremonies)
    }

    /// Deletes every session of `user` ("sign out everywhere", account deletion).
    #[allow(dead_code, reason = "used by account deletion (#22)")]
    pub async fn delete_all_for_user(&self, user: UserId) -> Result<u64, sqlx::Error> {
        Ok(
            sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user.as_uuid())
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }
}

/// Runs [`PgSessionStore::delete_expired`] every [`CLEANUP_INTERVAL`], starting after the first
/// interval. Abort the returned task on shutdown.
pub fn spawn_cleanup(store: PgSessionStore) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // The first tick is immediate: skip it.
        loop {
            interval.tick().await;
            match store.delete_expired().await {
                Ok(deleted) => tracing::info!(deleted, "deleted expired sessions and ceremonies"),
                Err(error) => tracing::warn!(%error, "cannot delete expired sessions"),
            }
        }
    })
}

/// The stored key for a session ID: SHA-256 of its cookie form. A leaked table cannot be turned
/// back into cookies.
fn id_hash(id: &Id) -> Vec<u8> {
    Sha256::digest(id.to_string().as_bytes()).to_vec()
}

/// The signed-in user recorded in the session data, for the `user_id` column.
fn record_user(record: &Record) -> Option<Uuid> {
    record
        .data
        .get(keys::USER_ID)
        .and_then(|value| serde_json::from_value::<UserId>(value.clone()).ok())
        .map(|user| user.as_uuid())
}

fn backend(error: sqlx::Error) -> session_store::Error {
    session_store::Error::Backend(error.to_string())
}

fn encode(record: &Record) -> session_store::Result<serde_json::Value> {
    serde_json::to_value(&record.data).map_err(|e| session_store::Error::Encode(e.to_string()))
}

/// How many times `create` draws a new ID after a collision (2^-128 each) before giving up.
const CREATE_ATTEMPTS: usize = 3;

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        let data = encode(record)?;
        let user = record_user(record);
        for _ in 0..CREATE_ATTEMPTS {
            let inserted = sqlx::query!(
                "INSERT INTO sessions (id_hash, user_id, data, expires_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (id_hash) DO NOTHING",
                id_hash(&record.id),
                user,
                data,
                record.expiry_date,
            )
            .execute(&self.pool)
            .await
            .map_err(backend)?
            .rows_affected();
            if inserted == 1 {
                return Ok(());
            }
            record.id = Id::default();
        }
        Err(session_store::Error::Backend(
            "could not allocate a unique session id".to_owned(),
        ))
    }

    /// Updates an existing, unexpired session. Never changes its `user_id` (set when the row is
    /// created: sign-in always creates a new session). Never inserts: a session deleted meanwhile (sign
    /// out in another tab, account deletion, expiry) must stay deleted, not be resurrected by a
    /// request that loaded it earlier.
    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let data = encode(record)?;
        let updated = sqlx::query!(
            "UPDATE sessions
             SET data = $2, expires_at = $3, updated_at = now()
             WHERE id_hash = $1 AND expires_at > now()",
            id_hash(&record.id),
            data,
            record.expiry_date,
        )
        .execute(&self.pool)
        .await
        .map_err(backend)?
        .rows_affected();
        if updated == 0 {
            tracing::debug!("session gone before it could be saved; not recreating it");
        }
        Ok(())
    }

    async fn load(&self, id: &Id) -> session_store::Result<Option<Record>> {
        let row = sqlx::query!(
            "SELECT data, expires_at FROM sessions WHERE id_hash = $1 AND expires_at > now()",
            id_hash(id),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        row.map(|row| {
            Ok(Record {
                id: *id,
                data: serde_json::from_value(row.data)
                    .map_err(|e| session_store::Error::Decode(e.to_string()))?,
                expiry_date: row.expires_at,
            })
        })
        .transpose()
    }

    async fn delete(&self, id: &Id) -> session_store::Result<()> {
        sqlx::query!("DELETE FROM sessions WHERE id_hash = $1", id_hash(id))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}

/// Unix seconds now.
#[must_use]
pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// Whether a session signed in at `signed_in_at` is past the absolute timeout at `now`.
/// A sign-in time in the future (clock skew, tampering) also counts as expired.
#[must_use]
pub fn absolute_expired(signed_in_at: i64, now: i64) -> bool {
    let max = i64::try_from(ABSOLUTE_TIMEOUT.as_secs()).unwrap_or(i64::MAX);
    signed_in_at > now || now.saturating_sub(signed_in_at) >= max
}

/// Whether the idle expiry should be pushed back, given the last refresh.
#[must_use]
pub fn needs_touch(last_seen_at: Option<i64>, now: i64) -> bool {
    let interval = i64::try_from(TOUCH_INTERVAL.as_secs()).unwrap_or(i64::MAX);
    last_seen_at.is_none_or(|last| last > now || now.saturating_sub(last) >= interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn absolute_timeout_boundaries() {
        let now = 1_000 * DAY;
        assert!(!absolute_expired(now, now));
        assert!(!absolute_expired(now - 30 * DAY + 1, now));
        assert!(absolute_expired(now - 30 * DAY, now));
        assert!(absolute_expired(i64::MIN, now));
    }

    #[test]
    fn a_sign_in_time_in_the_future_is_expired() {
        assert!(absolute_expired(101, 100));
    }

    #[test]
    fn touch_at_most_hourly() {
        let now = 1_000 * DAY;
        assert!(needs_touch(None, now));
        assert!(!needs_touch(Some(now), now));
        assert!(!needs_touch(Some(now - 3_599), now));
        assert!(needs_touch(Some(now - 3_600), now));
        assert!(needs_touch(Some(now + 10), now));
    }

    #[test]
    fn timeouts_are_ordered() {
        assert!(ANONYMOUS_TIMEOUT < TOUCH_INTERVAL);
        assert!(TOUCH_INTERVAL < IDLE_TIMEOUT);
        assert!(IDLE_TIMEOUT < ABSOLUTE_TIMEOUT);
    }

    #[test]
    fn cookie_name_uses_host_prefix_only_when_secure() {
        assert_eq!(cookie_name(true), "__Host-iron_oxide_session");
        assert_eq!(cookie_name(false), "iron_oxide_session");
    }

    #[test]
    fn id_hash_is_sha256_of_the_cookie_value() {
        let id = Id(42);
        let hash = id_hash(&id);
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, Sha256::digest(id.to_string().as_bytes()).to_vec());
        assert_ne!(hash, id_hash(&Id(43)));
    }

    #[test]
    fn record_user_reads_the_signed_in_user() {
        let user = UserId::from_uuid(Uuid::from_u128(7));
        let mut record = Record {
            id: Id(1),
            data: Default::default(),
            expiry_date: OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(record_user(&record), None);
        record.data.insert(
            keys::USER_ID.to_owned(),
            serde_json::to_value(user).unwrap(),
        );
        assert_eq!(record_user(&record), Some(user.as_uuid()));
        record
            .data
            .insert(keys::USER_ID.to_owned(), serde_json::json!("not a uuid"));
        assert_eq!(record_user(&record), None);
    }
}
