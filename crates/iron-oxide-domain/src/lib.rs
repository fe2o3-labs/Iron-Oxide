//! Pure domain logic for Iron Oxide: programs, sessions, progression, rest timer, plate maths.
//!
//! This crate must stay free of UI, web and database dependencies (no Dioxus, web-sys or sqlx),
//! so it builds and tests with plain `cargo test -p iron-oxide-domain` on any target.
//!
//! # Core types
//!
//! - Typed IDs: [`UserId`], [`SessionId`], [`SetId`], [`ProgramId`], [`ProgramVersionId`] (UUIDs) and
//!   [`ExerciseId`] and [`DayId`] (slugs).
//! - [`Weight`], stored exactly in kilograms, with [`Unit`] conversion at the edges, [`Rounding`] to an
//!   increment and display helpers.
//! - [`Reps`], [`Percent`] and [`Seconds`].
//! - [`ValueError`] for every rejected value.
//! - The rest timer lives in [`time`] and [`timer`].
//!
//! # Plate calculator
//!
//! [`calculate_plates`] loads a target on a bar from a [`PlateInventory`], exactly or as close as
//! possible from below and above.

pub mod session;
pub mod time;
pub mod timer;

mod display;
mod duration;
mod error;
mod ids;
mod percent;
mod plates;
pub mod program;
mod reps;
mod units;
mod weight;

pub use duration::Seconds;
pub use error::{Quantity, ValueError};
pub use ids::{
    DayId, ExerciseId, ProgramId, ProgramVersionId, SLUG_MAX_LEN, SessionId, SetId, UserId,
};
pub use percent::Percent;
pub use plates::{
    Loadout, PlateCount, PlateInventory, PlateInventoryError, PlateOutcome, PlateResult,
    PlateStock, calculate_plates,
};
pub use reps::Reps;
pub use session::{
    Change, LoggedSet, RotationError, Session, SessionError, SessionLog, SessionOutcome,
    SessionStatus, next_day,
};
pub use units::Unit;
pub use weight::{Rounding, Weight, WeightDisplay};
