//! Training sessions: what was trained, the sets logged, and which program day comes next.
//!
//! - [`Session`]: one session of a program day, linked to the immutable [`ProgramVersionId`] it
//!   was run from, with its [`SessionStatus`].
//! - [`LoggedSet`]: one set, keyed by a client-generated [`SetId`] so retried saves are idempotent.
//! - [`SessionLog`]: a session and its sets, with the invariants that keep them consistent. It is
//!   the in-progress state the client persists to localStorage.
//! - [`next_day`]: the day rotation rule.
//!
//! The types are generic over the timestamp type `T`, which only needs to be totally ordered and
//! `Copy`. The app uses UTC epoch milliseconds.
//!
//! [`ProgramVersionId`]: crate::ProgramVersionId
//! [`SetId`]: crate::SetId

mod day;
mod error;
mod log;
mod model;
mod rotation;
mod set;

pub use day::DayId;
pub use error::{RotationError, SessionError};
pub use log::SessionLog;
pub use model::{Change, Session, SessionOutcome, SessionStatus};
pub use rotation::next_day;
pub use set::LoggedSet;
