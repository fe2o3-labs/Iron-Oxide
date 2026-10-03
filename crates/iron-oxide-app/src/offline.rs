//! Resilience (#30): the session in progress kept on the device, and an outbox that delivers
//! the session writes when the server can be reached, in order and exactly as made.
//!
//! # For screens: write through the outbox
//!
//! Screens never call `start_session`, `save_set` or `finish_session` directly. They build the
//! write with fresh client ids and the device's clock, and enqueue it:
//!
//! ```ignore
//! let outbox = use_outbox();
//! let set = LoggedSet { id: SetId::new_v7(), completed_at: now, .. };
//! local_session.record_set(set.clone()); // and save it, see below
//! outbox.enqueue(Write::SaveSet { session_id, set })?; // Err(NotSignedIn) when signed out
//! ```
//!
//! - [`Outbox::enqueue`] stores the write on the device before it returns, then sends it in
//!   the background. Enqueueing the same write twice does nothing.
//! - Ids come from the domain's `new_v7()` (`SessionId`, `SetId`) and timestamps from the
//!   client. A retry resends exactly the same arguments, which the server answers unchanged
//!   (`docs/api.md`, "Idempotency"). Never regenerate an id or a time for a retry.
//! - Writes go out one at a time, oldest first, across sessions: a set never arrives before its
//!   session's start, and a finish always arrives before the next session's start.
//! - Results are not returned. A screen that needs one, such as the summary after
//!   `finish_session`, waits until [`Outbox::is_pending`] turns false, then calls the server
//!   function again with the same arguments. It is a replay, so it returns the same answer.
//!   Online, that is a fraction of a second.
//! - Reading the state: [`Outbox::status`] (or [`Outbox::pending_count`] and
//!   [`Outbox::last_error`]) is reactive. The [`crate::ui::unsaved::Unsaved`] indicator shows
//!   it whenever something is pending or failed.
//! - Rejected writes (`409`, `422`, …) stop the queue at that write, with the server's message
//!   in `last_error`. They are never dropped on their own. The user either fixes the cause and
//!   calls [`Outbox::retry_failed`], or gives the writes up with [`Outbox::discard_failed`].
//!   [`Outbox::retry_now`] skips the backoff.
//!
//! The session screen keeps a [`LocalSession`] and saves it after every change
//! (`LocalSession::save(storage, user)`, with [`platform::with_storage`]). On load it restores
//! the session, then reconciles with `get_in_progress_session`. It clears the session once the
//! finish is enqueued. Signing out clears it as well ([`Outbox::signed_out`]).
//!
//! # Delivery
//!
//! - Retryable failures (network, `429`, `502`-`504`) retry with exponential backoff and full
//!   jitter ([`backoff::Backoff::DEFAULT`]: 1 s doubling, capped at 5 min). A `429` is never
//!   retried before its `retry_after_secs`. The `online` event, the app start and a sign-in
//!   retry at once, but never before a `429`'s delay.
//! - `401` pauses the queue until the user signs in again. Before sending, the drain checks
//!   with `me()` that the browser's session is still the queue's user, so writes never land in
//!   another account.
//! - Everything else, `500` included (`docs/api.md` classifies it as not retryable), is a
//!   rejection.
//! - Single flight. In one tab a single task sends. Across tabs, a Web Lock
//!   (`navigator.locks`, `ifAvailable`) per user lets only one tab drain at a time. Without the
//!   Web Locks API (old browsers, insecure origins), two tabs may send the same write. The
//!   server answers the second copy unchanged, so the only cost is a request.
//! - Every change to the queue re-reads it from storage, applies the change and writes it back
//!   in one synchronous step, and the `storage` event refreshes the other tabs. This way tabs
//!   do not overwrite each other's writes.
//!
//! # Storage
//!
//! `localStorage`, per user (`iron-oxide:outbox:<user id>`, `iron-oxide:session:<user id>`),
//! versioned records ([`storage`]). Unreadable records or entries are moved to
//! `<key>:unreadable` rather than deleted. When storage is blocked or full, the outbox carries on
//! in memory and says so in `last_error`. A user's undelivered writes stay on the device after
//! sign-out and are sent at their next sign-in.
//!
//! The pure parts ([`backoff`], [`queue`], [`storage`], [`session`]) have no browser
//! dependency and are unit-tested on the host. [`platform`] holds the browser calls and
//! [`outbox`] the Dioxus glue.

// The screens that enqueue writes and keep the local session land with #28 and #29.
#![allow(dead_code)]

pub mod backoff;
pub mod outbox;
pub mod platform;
pub mod queue;
pub mod session;
pub mod storage;

#[allow(unused_imports, reason = "the API screens use (#28, #29)")]
pub use outbox::{NotSignedIn, Outbox, use_outbox, use_outbox_provider};
#[allow(unused_imports, reason = "the API screens use (#28, #29)")]
pub use queue::{OutboxStatus, Write, WriteKey};
#[allow(unused_imports, reason = "the API screens use (#28, #29)")]
pub use session::{LocalFinish, LocalSession};
