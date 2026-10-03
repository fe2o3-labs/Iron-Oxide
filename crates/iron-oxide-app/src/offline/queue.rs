//! The outbox's state machine: which write to send next, and what a send's result does to the
//! queue. Pure: no browser, no clock, no randomness (the caller passes `now` and the draw).
//!
//! - **One FIFO for all writes.** A write is only sent once everything enqueued before it was
//!   delivered, so the server sees them in the order the user made them: a session's sets after
//!   its start and before its finish, and a session's finish before the next session's start
//!   (which the server refuses while another session is in progress).
//! - **Retryable failures** (network, `429`, `502`-`504`) keep the write at the head and wait:
//!   [`Backoff`] with full jitter, never less than a `429`'s `Retry-After`. [`Queue::nudge`]
//!   (the browser came back online, the app started) skips the backoff but never a `Retry-After`.
//! - **Rejections** (`400`, `403`, `404`, `409`, `413`, `422`, `500`) mark the write failed and
//!   stop the queue there, with the server's message. Nothing is dropped: the user retries
//!   ([`Queue::retry_failed`]) or explicitly discards it ([`Queue::discard_failed`]).
//! - **`401`** pauses the queue without failing anything: every write waits for the user to sign
//!   in again, then [`Queue::nudge`] resumes.
//! - **Replays.** An entry is removed only after a `2xx`. Enqueueing a write that is already
//!   queued (same content) does nothing; a write is identified by the ids it carries
//!   ([`WriteKey`]), which the client generates with `new_v7()`, so the server recognises a
//!   resent write and answers it unchanged.

use std::collections::VecDeque;
use std::time::Duration;

use iron_oxide_domain::{LoggedSet, SessionId, SessionOutcome, SetId, time::Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::backoff::Backoff;

/// A write the outbox delivers: the exact arguments of one server function in
/// `crate::api::sessions`. Retries send them unchanged (ids and client timestamps included),
/// which is what makes them idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Write {
    /// `start_session(session_id, started_at)`.
    StartSession {
        session_id: SessionId,
        started_at: Timestamp,
    },
    /// `save_set(session_id, set)`.
    SaveSet {
        session_id: SessionId,
        set: LoggedSet<Timestamp>,
    },
    /// `finish_session(session_id, outcome, finished_at)`.
    FinishSession {
        session_id: SessionId,
        outcome: SessionOutcome,
        finished_at: Timestamp,
    },
}

impl Write {
    /// What identifies this write on the server.
    #[must_use]
    pub fn key(&self) -> WriteKey {
        match self {
            Self::StartSession { session_id, .. } => WriteKey::StartSession(*session_id),
            Self::SaveSet { set, .. } => WriteKey::SaveSet(set.id),
            Self::FinishSession { session_id, .. } => WriteKey::FinishSession(*session_id),
        }
    }

    /// The session the write belongs to.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        match self {
            Self::StartSession { session_id, .. }
            | Self::SaveSet { session_id, .. }
            | Self::FinishSession { session_id, .. } => *session_id,
        }
    }
}

/// What identifies a [`Write`]: the client-generated id the server deduplicates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum WriteKey {
    StartSession(SessionId),
    SaveSet(SetId),
    FinishSession(SessionId),
}

/// A queued write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub write: Write,
    /// When it was enqueued (the client's clock).
    pub enqueued_at: Timestamp,
    /// Set when the server rejected it (`409`, `422`, …): the message to show. The queue stops
    /// at a failed entry until it is retried or discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// What a failed send means for the queue, from `ApiFailure::classify` (see
/// `super::outbox::failure_of`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Network, `429`, `502`-`504`: send the same write again later. `retry_after` is a `429`'s
    /// delay: the next attempt never comes sooner.
    Retry {
        message: String,
        retry_after: Option<Duration>,
    },
    /// `401`: wait for the user to sign in again; nothing failed.
    SignedOut { message: String },
    /// Any other status: the server will never accept this exact write. Stop and show it.
    Rejected { message: String },
}

/// Whether [`Queue::enqueue`] added the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    Added,
    /// The same write is already queued: nothing changed.
    Duplicate,
}

/// When the queue wants to run next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A write is ready now.
    Now,
    /// The head write waits for its backoff or `Retry-After`.
    At(Timestamp),
    /// Nothing to do until something changes: empty, stopped at a failed write, or signed out.
    Idle,
}

/// What the "unsaved" indicator shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboxStatus {
    /// Writes not delivered yet, failed ones included.
    pub pending_count: usize,
    /// Writes the server rejected, waiting for the user.
    pub failed_count: usize,
    /// Why the queue is not empty, when something went wrong: a rejection's message first, then
    /// the sign-in pause, then the last retryable error. `None` while it simply waits for its turn.
    pub last_error: Option<String>,
}

impl OutboxStatus {
    /// Whether the indicator has anything to show.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.pending_count == 0 && self.last_error.is_none()
    }
}

/// The queue and its retry state, as persisted per user.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    entries: VecDeque<Entry>,
    /// Consecutive retryable failures of the head write.
    #[serde(default)]
    failures: u32,
    /// The backoff: no attempt before this time, unless nudged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_at: Option<Timestamp>,
    /// A `429`'s `Retry-After`: no attempt before this time, even when nudged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_before: Option<Timestamp>,
    /// Set by a `401`: the message to show until [`Queue::nudge`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signed_out: Option<String>,
    /// The last retryable error, cleared by the next success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

impl Queue {
    /// The queued writes, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends `write`, unless the same write is already queued.
    pub fn enqueue(&mut self, write: Write, now: Timestamp) -> Enqueued {
        if self.entries.iter().any(|entry| entry.write == write) {
            return Enqueued::Duplicate;
        }
        self.entries.push_back(Entry {
            write,
            enqueued_at: now,
            failed: None,
        });
        Enqueued::Added
    }

    /// When the queue wants to run next.
    #[must_use]
    pub fn wake(&self, now: Timestamp) -> Wake {
        let Some(head) = self.entries.front() else {
            return Wake::Idle;
        };
        if head.failed.is_some() || self.signed_out.is_some() {
            return Wake::Idle;
        }
        match self.retry_at.max(self.not_before) {
            Some(at) if at > now => Wake::At(at),
            _ => Wake::Now,
        }
    }

    /// The write to send now, if any: the head of the queue, once its wait is over.
    #[must_use]
    pub fn next_ready(&self, now: Timestamp) -> Option<&Write> {
        match self.wake(now) {
            Wake::Now => self.entries.front().map(|entry| &entry.write),
            Wake::At(_) | Wake::Idle => None,
        }
    }

    /// `write` was delivered (`2xx`): removes it and resets the retry state. Does nothing if it is
    /// no longer queued (another tab delivered it first).
    pub fn on_success(&mut self, write: &Write) {
        if let Some(index) = self.position(write) {
            self.entries.remove(index);
        }
        self.failures = 0;
        self.retry_at = None;
        self.not_before = None;
        self.last_error = None;
    }

    /// Sending `write` failed. `random` is a uniform draw in `[0, 1)` for the jitter.
    pub fn on_failure(
        &mut self,
        write: &Write,
        failure: Failure,
        now: Timestamp,
        random: f64,
        backoff: &Backoff,
    ) {
        let Some(index) = self.position(write) else {
            return;
        };
        match failure {
            Failure::Retry {
                message,
                retry_after,
            } => {
                self.failures = self.failures.saturating_add(1);
                let delay = backoff.next_delay(self.failures, random, retry_after);
                self.retry_at = Some(now.saturating_add(delay));
                self.not_before = retry_after.map(|after| now.saturating_add(after));
                self.last_error = Some(message);
            }
            Failure::SignedOut { message } => self.signed_out = Some(message),
            Failure::Rejected { message } => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.failed = Some(message);
                }
                self.failures = 0;
                self.retry_at = None;
                self.not_before = None;
                self.last_error = None;
            }
        }
    }

    /// Try again now: the browser is back online, the app started, the user signed in. Skips the
    /// backoff and the sign-in pause, never a `429`'s `Retry-After`. Failed writes stay failed.
    pub fn nudge(&mut self) {
        self.retry_at = None;
        self.signed_out = None;
    }

    /// Sends the failed writes again (the user asked to).
    pub fn retry_failed(&mut self) {
        for entry in &mut self.entries {
            entry.failed = None;
        }
        self.nudge();
    }

    /// Removes the failed writes (the user chose to give them up) and returns them.
    pub fn discard_failed(&mut self) -> Vec<Write> {
        let (failed, kept) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition::<Vec<_>, _>(|entry| entry.failed.is_some());
        self.entries = kept.into();
        failed.into_iter().map(|entry| entry.write).collect()
    }

    /// Whether `key` is still waiting to be delivered.
    #[must_use]
    pub fn contains(&self, key: WriteKey) -> bool {
        self.entries.iter().any(|entry| entry.write.key() == key)
    }

    /// What the indicator shows.
    #[must_use]
    pub fn status(&self) -> OutboxStatus {
        let failed = self
            .entries
            .iter()
            .filter_map(|entry| entry.failed.as_ref());
        let failed_count = failed.clone().count();
        let last_error = failed
            .into_iter()
            .next()
            .or(self.signed_out.as_ref())
            .or(self.last_error.as_ref())
            .filter(|_| !self.entries.is_empty())
            .cloned();
        OutboxStatus {
            pending_count: self.entries.len(),
            failed_count,
            last_error,
        }
    }

    fn position(&self, write: &Write) -> Option<usize> {
        self.entries.iter().position(|entry| entry.write == *write)
    }

    /// Reads a persisted queue, entry by entry: an entry that does not decode is returned apart
    /// (to be kept aside, never silently dropped), and the others are kept in order. Retry state
    /// that does not decode is reset.
    #[must_use]
    pub fn from_json(value: Value) -> (Self, Vec<Value>) {
        let Value::Object(mut fields) = value else {
            return (Self::default(), vec![value]);
        };
        let raw_entries = match fields.remove("entries") {
            Some(Value::Array(entries)) => entries,
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other],
        };
        let mut queue: Self = serde_json::from_value(Value::Object(fields)).unwrap_or_default();
        let mut rejected = Vec::new();
        for raw in raw_entries {
            match serde_json::from_value::<Entry>(raw.clone()) {
                Ok(entry) => queue.entries.push_back(entry),
                Err(_) => rejected.push(raw),
            }
        }
        (queue, rejected)
    }

    /// The persisted form.
    ///
    /// # Errors
    /// Never in practice: every field serializes to JSON.
    pub fn to_json(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iron_oxide_domain::{ExerciseId, Reps};

    pub(crate) fn at(millis: i64) -> Timestamp {
        Timestamp::from_epoch_millis(millis)
    }

    pub(crate) fn start(session: SessionId) -> Write {
        Write::StartSession {
            session_id: session,
            started_at: at(1_000),
        }
    }

    pub(crate) fn set(session: SessionId, index: u16) -> Write {
        Write::SaveSet {
            session_id: session,
            set: LoggedSet {
                id: SetId::new_v7(),
                exercise: "back-squat".parse::<ExerciseId>().unwrap(),
                set_index: index,
                reps: Reps::new(5),
                weight: None,
                duration: None,
                warm_up: false,
                completed_at: at(2_000 + i64::from(index)),
            },
        }
    }

    pub(crate) fn finish(session: SessionId) -> Write {
        Write::FinishSession {
            session_id: session,
            outcome: SessionOutcome::Completed,
            finished_at: at(9_000),
        }
    }

    fn retry(message: &str) -> Failure {
        Failure::Retry {
            message: message.to_owned(),
            retry_after: None,
        }
    }

    fn rejected(message: &str) -> Failure {
        Failure::Rejected {
            message: message.to_owned(),
        }
    }

    const B: Backoff = Backoff::DEFAULT;

    /// Sends every ready write in order, all succeeding, and returns them.
    fn deliver_all(queue: &mut Queue, now: Timestamp) -> Vec<Write> {
        let mut sent = Vec::new();
        while let Some(write) = queue.next_ready(now).cloned() {
            queue.on_success(&write);
            sent.push(write);
        }
        sent
    }

    #[test]
    fn writes_are_delivered_in_fifo_order() {
        let (a, b) = (SessionId::new_v7(), SessionId::new_v7());
        let writes = vec![
            start(a),
            set(a, 0),
            set(a, 1),
            finish(a),
            start(b),
            set(b, 0),
        ];
        let mut queue = Queue::default();
        for write in &writes {
            assert_eq!(queue.enqueue(write.clone(), at(0)), Enqueued::Added);
        }
        assert_eq!(queue.status().pending_count, 6);
        assert_eq!(deliver_all(&mut queue, at(0)), writes);
        assert!(queue.is_empty());
        assert_eq!(queue.status(), OutboxStatus::default());
        assert!(queue.status().is_clean());
    }

    #[test]
    fn a_retryable_failure_keeps_the_head_and_waits_for_the_backoff() {
        let session = SessionId::new_v7();
        let mut queue = Queue::default();
        let second = set(session, 0);
        queue.enqueue(start(session), at(0));
        queue.enqueue(second.clone(), at(0));
        let head = queue.next_ready(at(0)).cloned().unwrap();
        assert_eq!(head, start(session));

        queue.on_failure(&head, retry("Cannot reach the server."), at(0), 1.0, &B);
        assert_eq!(queue.wake(at(0)), Wake::At(at(1_000)));
        assert_eq!(queue.next_ready(at(999)), None);
        assert_eq!(queue.next_ready(at(1_000)), Some(&head));
        let status = queue.status();
        assert_eq!(status.pending_count, 2);
        assert_eq!(status.failed_count, 0);
        assert_eq!(
            status.last_error.as_deref(),
            Some("Cannot reach the server.")
        );

        // The second failure doubles the ceiling.
        queue.on_failure(&head, retry("x"), at(1_000), 1.0, &B);
        assert_eq!(queue.wake(at(1_000)), Wake::At(at(3_000)));

        // Delivered: the retry state resets and the next write follows.
        queue.on_success(&head);
        assert_eq!(queue.next_ready(at(1_001)), Some(&second));
        assert_eq!(queue.status().last_error, None);
        assert_eq!(queue.wake(at(1_001)), Wake::Now);
    }

    #[test]
    fn retry_after_takes_precedence_and_survives_a_nudge() {
        let session = SessionId::new_v7();
        let mut queue = Queue::default();
        queue.enqueue(start(session), at(0));
        let head = start(session);
        queue.on_failure(
            &head,
            Failure::Retry {
                message: "Too many requests.".to_owned(),
                retry_after: Some(Duration::from_secs(30)),
            },
            at(0),
            0.0,
            &B,
        );
        assert_eq!(queue.wake(at(0)), Wake::At(at(30_000)));
        // Online again, or the app restarted: the 429's delay still holds.
        queue.nudge();
        assert_eq!(queue.wake(at(10_000)), Wake::At(at(30_000)));
        assert_eq!(queue.next_ready(at(30_000)), Some(&head));
    }

    #[test]
    fn a_nudge_skips_the_backoff() {
        let session = SessionId::new_v7();
        let mut queue = Queue::default();
        queue.enqueue(start(session), at(0));
        queue.on_failure(&start(session), retry("offline"), at(0), 1.0, &B);
        for _ in 0..8 {
            queue.on_failure(&start(session), retry("offline"), at(0), 1.0, &B);
        }
        assert_eq!(queue.wake(at(0)), Wake::At(at(256_000)));
        queue.nudge();
        assert_eq!(queue.wake(at(0)), Wake::Now);
    }

    #[test]
    fn a_rejection_stops_the_queue_and_keeps_the_write() {
        let session = SessionId::new_v7();
        let (first, second) = (set(session, 0), set(session, 1));
        let mut queue = Queue::default();
        queue.enqueue(first.clone(), at(0));
        queue.enqueue(second.clone(), at(0));
        queue.on_failure(
            &first,
            rejected("This session has already ended."),
            at(0),
            0.5,
            &B,
        );

        // Stopped: nothing is sent, not even the next write, and nothing was dropped.
        assert_eq!(queue.wake(at(0)), Wake::Idle);
        assert_eq!(queue.next_ready(at(1_000_000)), None);
        queue.nudge();
        assert_eq!(queue.next_ready(at(1_000_000)), None);
        let status = queue.status();
        assert_eq!(status.pending_count, 2);
        assert_eq!(status.failed_count, 1);
        assert_eq!(
            status.last_error.as_deref(),
            Some("This session has already ended.")
        );
        assert!(!status.is_clean());

        // The user retries: the same write is sent again, then the rest in order.
        queue.retry_failed();
        assert_eq!(deliver_all(&mut queue, at(0)), vec![first, second]);
    }

    #[test]
    fn discarding_removes_only_the_failed_writes() {
        let session = SessionId::new_v7();
        let (first, second) = (set(session, 0), set(session, 1));
        let mut queue = Queue::default();
        queue.enqueue(first.clone(), at(0));
        queue.enqueue(second.clone(), at(0));
        queue.on_failure(
            &first,
            rejected("Some values are not valid."),
            at(0),
            0.5,
            &B,
        );
        assert_eq!(queue.discard_failed(), vec![first]);
        assert_eq!(deliver_all(&mut queue, at(0)), vec![second]);
    }

    #[test]
    fn signed_out_pauses_without_failing_anything() {
        let session = SessionId::new_v7();
        let mut queue = Queue::default();
        queue.enqueue(start(session), at(0));
        queue.on_failure(
            &start(session),
            Failure::SignedOut {
                message: "Please sign in.".to_owned(),
            },
            at(0),
            0.5,
            &B,
        );
        assert_eq!(queue.wake(at(1_000_000)), Wake::Idle);
        let status = queue.status();
        assert_eq!(status.failed_count, 0);
        assert_eq!(status.last_error.as_deref(), Some("Please sign in."));
        // Signed in again.
        queue.nudge();
        assert_eq!(deliver_all(&mut queue, at(0)), vec![start(session)]);
    }

    #[test]
    fn the_same_write_is_queued_once_and_replays_are_harmless() {
        let session = SessionId::new_v7();
        let write = set(session, 0);
        let mut queue = Queue::default();
        assert_eq!(queue.enqueue(write.clone(), at(0)), Enqueued::Added);
        assert_eq!(queue.enqueue(write.clone(), at(5)), Enqueued::Duplicate);
        assert!(queue.contains(write.key()));
        assert_eq!(queue.status().pending_count, 1);

        // The answer was lost (a 503 after the commit): the very same write, same id, is sent again.
        let sent = queue.next_ready(at(0)).cloned().unwrap();
        queue.on_failure(&sent, retry("busy"), at(0), 0.0, &B);
        let resent = queue.next_ready(at(0)).cloned().unwrap();
        assert_eq!(resent, sent);
        assert_eq!(resent.key(), write.key());
        queue.on_success(&resent);
        // Another tab delivered it too: a second success changes nothing.
        queue.on_success(&resent);
        assert!(queue.is_empty());
        assert!(!queue.contains(write.key()));
    }

    #[test]
    fn keys_identify_writes_by_their_client_ids() {
        let session = SessionId::new_v7();
        assert_eq!(start(session).key(), WriteKey::StartSession(session));
        assert_eq!(finish(session).key(), WriteKey::FinishSession(session));
        let Write::SaveSet { set: logged, .. } = set(session, 3) else {
            unreachable!()
        };
        let write = Write::SaveSet {
            session_id: session,
            set: logged.clone(),
        };
        assert_eq!(write.key(), WriteKey::SaveSet(logged.id));
        assert_eq!(write.session_id(), session);
    }

    #[test]
    fn the_queue_round_trips_through_json() {
        let session = SessionId::new_v7();
        let mut queue = Queue::default();
        queue.enqueue(start(session), at(0));
        queue.enqueue(set(session, 0), at(1));
        queue.enqueue(finish(session), at(2));
        queue.on_failure(&start(session), retry("offline"), at(3), 0.5, &B);
        let (decoded, rejected) = Queue::from_json(queue.to_json().unwrap());
        assert!(rejected.is_empty());
        assert_eq!(decoded, queue);
    }

    #[test]
    fn entries_that_do_not_decode_are_set_aside_and_the_rest_kept_in_order() {
        let session = SessionId::new_v7();
        let (first, last) = (start(session), finish(session));
        let mut json = serde_json::json!({
            "entries": [
                serde_json::to_value(Entry { write: first.clone(), enqueued_at: at(0), failed: None }).unwrap(),
                { "write": { "kind": "teleport" }, "enqueued_at": 1 },
                42,
                serde_json::to_value(Entry { write: last.clone(), enqueued_at: at(2), failed: None }).unwrap(),
            ],
            "failures": "many",
        });
        let (queue, rejected) = Queue::from_json(json.take());
        assert_eq!(rejected.len(), 2);
        assert_eq!(
            queue
                .entries()
                .map(|entry| entry.write.clone())
                .collect::<Vec<_>>(),
            vec![first, last]
        );
        // The retry state did not decode: it starts afresh.
        assert_eq!(queue.wake(at(0)), Wake::Now);

        let (queue, rejected) = Queue::from_json(serde_json::json!("garbage"));
        assert!(queue.is_empty());
        assert_eq!(rejected, vec![serde_json::json!("garbage")]);
    }
}
