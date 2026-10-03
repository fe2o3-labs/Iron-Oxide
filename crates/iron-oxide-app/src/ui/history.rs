//! History (#33): the list of past sessions, one session's details, and an exercise's progress
//! charts.
//!
//! - `list`: the sessions, a page at a time, and the exercises with a chart.
//! - `details`: one session's exercises and sets.
//! - `progress`: one exercise's charts (top set and estimated 1RM over time), a Pro feature
//!   (`Feature::ExerciseCharts`): without it the screen shows a locked card. The list and the
//!   details are never gated (`docs/billing.md`).
//! - `chart`: the charts' geometry (scales, ticks, the line), pure and tested.
//! - `view`: what the screens show as plain data (dates, durations, set lines, names), tested.
//! - `local_time`: the user's time zone offset, for dates.
//!
//! [`HistoryLayout`] wraps the three screens. It loads, once while the user is in the history,
//! what they share: the user's unit, whether charts are included in the plan, and the day and
//! exercise names of the programs (sessions only carry slugs).

mod chart;
mod details;
mod list;
mod local_time;
mod progress;
mod view;

use dioxus::prelude::*;
use iron_oxide_domain::{ProgramId, entitlements::Feature};

use super::errors::{Errors, use_errors};
use super::shell::Route;
use super::weight::UnitSetting;
use crate::api::billing::my_entitlements;
use crate::api::programs::{get_active_program, get_program};
use crate::api::settings::get_settings;
pub use details::HistorySession;
pub use list::History;
pub use progress::ExerciseProgress;
use view::{LocalDate, Names};

/// Whether the user's plan includes the progress charts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartAccess {
    /// Not known yet.
    Checking,
    Included,
    /// Not in the user's plan: the locked card is shown.
    Locked,
    /// The plan could not be read (the error is in the banner).
    Unknown,
}

/// What the history screens share, provided by [`HistoryLayout`].
#[derive(Clone, Copy, PartialEq)]
pub struct HistoryContext {
    pub names: Signal<Names>,
    pub charts: Signal<ChartAccess>,
}

impl HistoryContext {
    /// Loads the day and exercise names of the programs among `ids` not loaded yet (each once).
    /// A failure is reported; the slugs stand in for the names.
    pub fn learn_programs(self, ids: impl IntoIterator<Item = ProgramId>, errors: Errors) {
        let mut names = self.names;
        let wanted = names.write().claim_unrequested(ids);
        for program_id in wanted {
            spawn(async move {
                match get_program(program_id).await {
                    Ok(detail) => names.write().learn(program_id, &detail.document),
                    Err(error) => errors.report(&error),
                }
            });
        }
    }
}

impl HistoryContext {
    /// Asks the server whether the plan includes the charts (again, after a failure).
    pub fn check_plan(self, errors: Errors) {
        let mut charts = self.charts;
        charts.set(ChartAccess::Checking);
        spawn(async move {
            match my_entitlements().await {
                Ok(entitlements) => charts.set(if entitlements.allows(Feature::ExerciseCharts) {
                    ChartAccess::Included
                } else {
                    ChartAccess::Locked
                }),
                Err(error) => {
                    errors.report(&error);
                    charts.set(ChartAccess::Unknown);
                }
            }
        });
    }
}

/// The history screens' context.
#[must_use]
pub fn use_history() -> HistoryContext {
    use_context::<HistoryContext>()
}

/// The layout of the history screens: loads what they share, then shows the screen.
#[component]
pub fn HistoryLayout() -> Element {
    let errors = use_errors();
    let names = use_signal(Names::default);
    let charts = use_signal(|| ChartAccess::Checking);
    let context = use_context_provider(|| HistoryContext { names, charts });
    let unit = use_context::<UnitSetting>().0;

    use_hook(move || {
        if !cfg!(feature = "web") {
            return;
        }
        context.check_plan(errors);
        // Weights in the user's unit. The settings screen (#34) keeps it up to date afterwards.
        spawn(async move {
            let mut unit = unit;
            match get_settings().await {
                Ok(settings) => {
                    if *unit.peek() != settings.unit {
                        unit.set(settings.unit);
                    }
                }
                Err(error) => errors.report(&error),
            }
        });
        // The active program names most exercises, including on the chart screens (an exercise's
        // series does not say which program it came from).
        spawn(async move {
            match get_active_program().await {
                Ok(Some(detail)) => {
                    let mut names = context.names;
                    names.write().learn(detail.program.id, &detail.document);
                }
                Ok(None) => {}
                Err(error) => errors.report(&error),
            }
        });
    });

    rsx! { Outlet::<Route> {} }
}

/// The local date of a time (ms since the epoch).
#[must_use]
fn local_date(at_ms: i64) -> LocalDate {
    LocalDate::of(at_ms, local_time::offset_minutes(at_ms))
}

/// A link back to the session list.
#[component]
fn BackToHistory() -> Element {
    rsx! {
        Link { class: "io-back", to: Route::History {}, "← History" }
    }
}
