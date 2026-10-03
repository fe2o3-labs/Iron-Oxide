//! Versioned, per-user records in a key-value store (`localStorage` in the browser, memory
//! elsewhere and when it is unavailable), tolerant of missing, unreadable and full storage.
//!
//! Every record is `{"v": <version>, "data": …}` under a key that names the user:
//! `iron-oxide:<record>:<user id>`. A record that does not parse, or has a version this build
//! does not read, is moved aside to `<key>:unreadable` (never deleted) and reads as missing.
//! Bump the record's version when its shape changes incompatibly, and teach its reader the old
//! one if the data is worth keeping.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use dioxus::logger::tracing;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::queue::{OutboxStatus, Queue};
use crate::auth::types::UserId;

/// A failed storage call: storage blocked (private mode, disabled cookies) or full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError(pub String);

/// A string key-value store with `localStorage`'s semantics.
pub trait Storage {
    /// The value under `key`, if any.
    ///
    /// # Errors
    /// When the storage cannot be read.
    fn get(&self, key: &str) -> Result<Option<String>, StorageError>;

    /// Stores `value` under `key`.
    ///
    /// # Errors
    /// When the storage cannot be written (full, blocked).
    fn set(&self, key: &str, value: &str) -> Result<(), StorageError>;

    /// Removes `key`.
    ///
    /// # Errors
    /// When the storage cannot be written.
    fn remove(&self, key: &str) -> Result<(), StorageError>;
}

/// An in-memory store: the fallback when `localStorage` is unavailable, and the tests' store.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    items: RefCell<BTreeMap<String, String>>,
    /// Test switch: every write fails, as with a full `localStorage`.
    full: Cell<bool>,
}

impl MemoryStorage {
    /// Makes every write fail (`true`) or succeed again.
    #[cfg(test)]
    pub fn set_full(&self, full: bool) {
        self.full.set(full);
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self.items.borrow().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), StorageError> {
        if self.full.get() {
            return Err(StorageError("QuotaExceededError".to_owned()));
        }
        self.items
            .borrow_mut()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<(), StorageError> {
        self.items.borrow_mut().remove(key);
        Ok(())
    }
}

/// The record under the user's key.
#[must_use]
pub fn key(record: &str, user: UserId) -> String {
    format!("iron-oxide:{record}:{user}")
}

/// The key the unreadable data of `key` is moved to.
#[must_use]
pub fn unreadable_key(key: &str) -> String {
    format!("{key}:unreadable")
}

/// How many unreadable records are kept aside per key (the oldest go first).
const MAX_UNREADABLE: usize = 10;

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u32,
    data: Value,
}

/// Reads the record under `key` written with `version`. Missing data is `Ok(None)`; unreadable
/// data or another version is moved aside ([`unreadable_key`]) and also reads as `Ok(None)`.
///
/// # Errors
/// When the storage cannot be read.
pub fn read(storage: &dyn Storage, key: &str, version: u32) -> Result<Option<Value>, StorageError> {
    let Some(raw) = storage.get(key)? else {
        return Ok(None);
    };
    match serde_json::from_str::<Envelope>(&raw) {
        Ok(envelope) if envelope.v == version => Ok(Some(envelope.data)),
        Ok(envelope) => {
            tracing::warn!(
                key,
                version = envelope.v,
                "unknown record version, kept aside"
            );
            set_aside(storage, key, Value::String(raw));
            Ok(None)
        }
        Err(error) => {
            tracing::warn!(key, %error, "unreadable record, kept aside");
            set_aside(storage, key, Value::String(raw));
            Ok(None)
        }
    }
}

/// Writes `data` under `key` as `version`.
///
/// # Errors
/// When the storage cannot be written (full, blocked).
pub fn write(
    storage: &dyn Storage,
    key: &str,
    version: u32,
    data: Value,
) -> Result<(), StorageError> {
    let raw = serde_json::to_string(&Envelope { v: version, data })
        .map_err(|error| StorageError(error.to_string()))?;
    storage.set(key, &raw)
}

/// Keeps `data` under [`unreadable_key`] (best effort: a full storage loses it, with a warning),
/// and removes `key`.
pub fn set_aside(storage: &dyn Storage, key: &str, data: Value) {
    let aside = unreadable_key(key);
    let mut kept: Vec<Value> = storage
        .get(&aside)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    kept.push(data);
    let excess = kept.len().saturating_sub(MAX_UNREADABLE);
    kept.drain(..excess);
    let stored = serde_json::to_string(&kept)
        .map_err(|error| StorageError(error.to_string()))
        .and_then(|raw| storage.set(&aside, &raw));
    if let Err(error) = stored {
        tracing::warn!(key, ?error, "could not keep the unreadable record aside");
    }
    let _ = storage.remove(key);
}

/// Shown while the outbox cannot be saved on the device.
pub const NOT_PERSISTED_MESSAGE: &str =
    "Not saved on this device yet. Keep the app open until it syncs.";

/// One user's outbox in storage. Every change reads the stored queue, applies the change and
/// writes it back in the same synchronous step, so writes queued by another tab are kept.
///
/// When storage fails (blocked, full), the queue lives on in memory: changes keep working for as
/// long as the page is open, and [`OutboxStatus::last_error`] says they are not on the device.
#[derive(Debug)]
pub struct QueueStore {
    key: String,
    /// The queue as last seen. Authoritative while `not_persisted`.
    mirror: Queue,
    not_persisted: bool,
}

impl QueueStore {
    /// The version of the outbox record.
    pub const VERSION: u32 = 1;

    #[must_use]
    pub fn new(user: UserId) -> Self {
        Self {
            key: key("outbox", user),
            mirror: Queue::default(),
            not_persisted: false,
        }
    }

    /// The storage key, to recognise changes made by other tabs.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The current queue: the stored one, or the memory copy while storage fails. Entries that do
    /// not decode are moved aside.
    pub fn load(&mut self, storage: &dyn Storage) -> &Queue {
        if !self.not_persisted {
            match read(storage, &self.key, Self::VERSION) {
                Ok(Some(data)) => {
                    let (queue, unreadable) = Queue::from_json(data);
                    self.mirror = queue;
                    if !unreadable.is_empty() {
                        tracing::warn!(
                            key = self.key,
                            count = unreadable.len(),
                            "unreadable outbox entries, kept aside"
                        );
                        for entry in unreadable {
                            set_aside(storage, &self.key, entry);
                        }
                        self.save(storage);
                    }
                }
                Ok(None) => self.mirror = Queue::default(),
                // Storage blocked: keep using the memory copy.
                Err(_) => self.not_persisted = true,
            }
        }
        &self.mirror
    }

    /// Applies `change` to the current queue and stores the result.
    pub fn update<R>(&mut self, storage: &dyn Storage, change: impl FnOnce(&mut Queue) -> R) -> R {
        self.load(storage);
        let result = change(&mut self.mirror);
        self.save(storage);
        result
    }

    /// The queue as last loaded or changed.
    #[must_use]
    pub const fn queue(&self) -> &Queue {
        &self.mirror
    }

    /// What the indicator shows.
    #[must_use]
    pub fn status(&self) -> OutboxStatus {
        let mut status = self.mirror.status();
        if self.not_persisted && status.pending_count > 0 && status.last_error.is_none() {
            status.last_error = Some(NOT_PERSISTED_MESSAGE.to_owned());
        }
        status
    }

    fn save(&mut self, storage: &dyn Storage) {
        let saved = if self.mirror.is_empty() {
            storage.remove(&self.key)
        } else {
            self.mirror
                .to_json()
                .map_err(|error| StorageError(error.to_string()))
                .and_then(|data| write(storage, &self.key, Self::VERSION, data))
        };
        match saved {
            Ok(()) => self.not_persisted = false,
            Err(error) => {
                if !self.not_persisted {
                    tracing::warn!(key = self.key, ?error, "outbox kept in memory only");
                }
                self.not_persisted = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::queue::tests::{at, set, start};
    use iron_oxide_domain::SessionId;

    fn user() -> UserId {
        UserId::from_uuid(uuid::Uuid::from_u128(7))
    }

    #[test]
    fn keys_are_per_user() {
        let other = UserId::from_uuid(uuid::Uuid::from_u128(8));
        assert_eq!(
            key("outbox", user()),
            "iron-oxide:outbox:00000000-0000-0000-0000-000000000007"
        );
        assert_ne!(key("outbox", user()), key("outbox", other));
        assert_ne!(key("outbox", user()), key("session", user()));
    }

    #[test]
    fn a_record_round_trips_with_its_version() {
        let storage = MemoryStorage::default();
        assert_eq!(read(&storage, "k", 1), Ok(None));
        write(&storage, "k", 1, serde_json::json!({"a": 1})).unwrap();
        assert_eq!(
            read(&storage, "k", 1),
            Ok(Some(serde_json::json!({"a": 1})))
        );
    }

    #[test]
    fn corrupt_or_other_version_data_is_kept_aside_and_reads_as_missing() {
        let storage = MemoryStorage::default();
        storage.set("k", "{not json").unwrap();
        assert_eq!(read(&storage, "k", 1), Ok(None));
        assert_eq!(storage.get("k"), Ok(None));
        write(&storage, "k", 2, serde_json::json!([])).unwrap();
        assert_eq!(read(&storage, "k", 1), Ok(None));
        let aside: Vec<Value> =
            serde_json::from_str(&storage.get("k:unreadable").unwrap().unwrap()).unwrap();
        assert_eq!(aside.len(), 2);
        assert_eq!(aside[0], Value::String("{not json".to_owned()));
        // Bounded.
        for _ in 0..20 {
            storage.set("k", "x").unwrap();
            assert_eq!(read(&storage, "k", 1), Ok(None));
        }
        let aside: Vec<Value> =
            serde_json::from_str(&storage.get("k:unreadable").unwrap().unwrap()).unwrap();
        assert_eq!(aside.len(), MAX_UNREADABLE);
    }

    #[test]
    fn the_outbox_recovers_from_corrupt_storage() {
        let storage = MemoryStorage::default();
        let mut store = QueueStore::new(user());
        storage.set(store.key(), "\u{0}garbage").unwrap();
        assert!(store.load(&storage).is_empty());
        // Usable again, and the garbage is kept aside.
        let session = SessionId::new_v7();
        store.update(&storage, |queue| queue.enqueue(start(session), at(0)));
        let mut fresh = QueueStore::new(user());
        assert_eq!(fresh.load(&storage).status().pending_count, 1);
        assert!(storage.get(&unreadable_key(store.key())).unwrap().is_some());
    }

    #[test]
    fn unreadable_entries_are_kept_aside_and_the_others_kept() {
        let storage = MemoryStorage::default();
        let mut store = QueueStore::new(user());
        let session = SessionId::new_v7();
        store.update(&storage, |queue| {
            queue.enqueue(start(session), at(0));
            queue.enqueue(set(session, 0), at(0));
        });
        let mut raw: Value =
            serde_json::from_str(&storage.get(store.key()).unwrap().unwrap()).unwrap();
        raw["data"]["entries"][0] = serde_json::json!({"write": "?"});
        storage.set(store.key(), &raw.to_string()).unwrap();

        let mut fresh = QueueStore::new(user());
        assert_eq!(fresh.load(&storage).status().pending_count, 1);
        // The repair is stored, so the bad entry is set aside once.
        let mut again = QueueStore::new(user());
        assert_eq!(again.load(&storage).status().pending_count, 1);
        let aside: Vec<Value> =
            serde_json::from_str(&storage.get(&unreadable_key(store.key())).unwrap().unwrap())
                .unwrap();
        assert_eq!(aside, vec![serde_json::json!({"write": "?"})]);
    }

    #[test]
    fn changes_from_another_tab_are_kept() {
        let storage = MemoryStorage::default();
        let (mut tab_a, mut tab_b) = (QueueStore::new(user()), QueueStore::new(user()));
        let session = SessionId::new_v7();
        tab_a.load(&storage);
        tab_b.update(&storage, |queue| queue.enqueue(start(session), at(0)));
        tab_a.update(&storage, |queue| queue.enqueue(set(session, 0), at(1)));
        assert_eq!(tab_b.load(&storage).status().pending_count, 2);
    }

    #[test]
    fn a_full_storage_keeps_the_outbox_in_memory_and_says_so() {
        let storage = MemoryStorage::default();
        let mut store = QueueStore::new(user());
        let session = SessionId::new_v7();
        storage.set_full(true);
        store.update(&storage, |queue| queue.enqueue(start(session), at(0)));
        store.update(&storage, |queue| queue.enqueue(set(session, 0), at(0)));
        let status = store.status();
        assert_eq!(status.pending_count, 2);
        assert_eq!(status.last_error.as_deref(), Some(NOT_PERSISTED_MESSAGE));

        // Space again: the next change stores everything.
        storage.set_full(false);
        store.update(&storage, |queue| queue.enqueue(set(session, 1), at(0)));
        assert_eq!(store.status().last_error, None);
        assert_eq!(
            QueueStore::new(user())
                .load(&storage)
                .status()
                .pending_count,
            3
        );
    }

    #[test]
    fn an_empty_outbox_leaves_nothing_in_storage() {
        let storage = MemoryStorage::default();
        let mut store = QueueStore::new(user());
        let session = SessionId::new_v7();
        store.update(&storage, |queue| queue.enqueue(start(session), at(0)));
        store.update(&storage, |queue| queue.on_success(&start(session)));
        assert_eq!(storage.get(store.key()), Ok(None));
    }
}
