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
//! # Strength statistics
//!
//! e1RM ([`E1rmFormula`]), [`Volume`], [`top_set`], personal records ([`ExerciseRecords`],
//! [`PrEvent`]) and chart series ([`exercise_series`]), all computed from [`PerformedSet`] values.

pub mod time;
pub mod timer;

mod display;
mod duration;
mod error;
mod ids;
mod percent;
mod reps;
mod stats;
mod units;
mod weight;

pub use duration::Seconds;
pub use error::{Quantity, ValueError};
pub use ids::{
    DayId, ExerciseId, ProgramId, ProgramVersionId, SLUG_MAX_LEN, SessionId, SetId, UserId,
};
pub use percent::Percent;
pub use reps::Reps;
pub use stats::{
    E1rmFormula, ExerciseRecords, Lift, MAX_E1RM_REPS, PerformedSet, PrEvent, PrKind, SeriesPoint,
    Volume, VolumeDisplay, detect_prs, estimate_1rm, exercise_series, session_volume, top_set,
};
pub use units::Unit;
pub use weight::{Rounding, Weight, WeightDisplay};
