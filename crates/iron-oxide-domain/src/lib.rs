//! Pure domain logic for Iron Oxide: programs, sessions, progression, rest timer, plate maths.
//!
//! This crate must stay free of UI, web and database dependencies (no Dioxus, web-sys or sqlx),
//! so it builds and tests with plain `cargo test -p iron-oxide-domain` on any target.
//!
//! # Core types
//!
//! - Typed IDs: [`UserId`], [`SessionId`], [`SetId`], [`ProgramId`], [`ProgramVersionId`] (UUIDs) and
//!   [`ExerciseId`] (a slug).
//! - [`Weight`], stored exactly in kilograms, with [`Unit`] conversion at the edges, [`Rounding`] to an
//!   increment and display helpers.
//! - [`Reps`], [`Percent`] and [`Seconds`].
//! - [`ValueError`] for every rejected value.
//!
//! # Plate calculator
//!
//! [`calculate_plates`] loads a target on a bar from a [`PlateInventory`], exactly or as close as
//! possible from below and above.

mod duration;
mod error;
mod ids;
mod percent;
mod plates;
mod reps;
mod units;
mod weight;

pub use duration::Seconds;
pub use error::{Quantity, ValueError};
pub use ids::{ExerciseId, ProgramId, ProgramVersionId, SessionId, SetId, UserId};
pub use percent::Percent;
pub use plates::{
    Loadout, PlateCount, PlateInventory, PlateInventoryError, PlateOutcome, PlateResult,
    PlateStock, calculate_plates,
};
pub use reps::Reps;
pub use units::Unit;
pub use weight::{Rounding, Weight, WeightDisplay};
