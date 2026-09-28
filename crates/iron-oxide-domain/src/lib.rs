//! Pure domain logic for Iron Oxide: no UI, no I/O, no database.
//!
//! This crate must not depend on Dioxus, web-sys or sqlx, so that it builds for the server and for
//! `wasm32-unknown-unknown` alike and its tests run with plain `cargo test`.
//!
//! # Core types
//!
//! - Typed IDs: [`UserId`], [`SessionId`], [`SetId`], [`ProgramId`], [`ProgramVersionId`] (UUIDs) and
//!   [`ExerciseId`] (a slug).
//! - [`Weight`], stored exactly in kilograms, with [`Unit`] conversion at the edges, [`Rounding`] to an
//!   increment and display helpers.
//! - [`Reps`], [`Percent`] and [`Seconds`].
//! - [`ValueError`] for every rejected value.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod duration;
mod error;
mod ids;
mod percent;
mod reps;
mod units;
mod weight;

pub use duration::Seconds;
pub use error::{Quantity, ValueError};
pub use ids::{ExerciseId, ProgramId, ProgramVersionId, SessionId, SetId, UserId};
pub use percent::Percent;
pub use reps::Reps;
pub use units::Unit;
pub use weight::{Rounding, Weight, WeightDisplay};
