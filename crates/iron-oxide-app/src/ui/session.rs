//! The workout session (#28): start the next workout, log it one set at a time, finish it.
//!
//! - `flow`: the pure view model (order of the sets, prefill, labels), unit-tested.
//! - `writes`: every session write (start, save a set, finish), in one file so the offline outbox
//!   (#30) can take them over.
//! - `workout`: the active session screen.
//! - `plates`: the slot for the plate calculator (#31).
//! - `rest`: the rest timer between sets (#29).
//! - `sheet`: the bottom sheet for confirmations.
//! - `platform`: the browser clock, `localStorage`, sound, vibration and the screen wake lock.
//!
//! The page loads on the client only (like the shell's sign-in check), so the server render shows
//! the loading state and hydration matches. An in-progress session is resumed from the server: its
//! plan and the sets already saved, so a reload continues at the next set.

mod flow;
mod plates;
mod platform;
mod rest;
mod sheet;
mod workout;
mod writes;

use std::collections::BTreeSet;

use dioxus::prelude::*;
use iron_oxide_domain::time::Timestamp;
use iron_oxide_domain::{ExerciseId, LoggedSet, SessionId};

use crate::api::error::{ApiFailure, FailureKind};
use crate::api::sessions::{
    NextSessionPlan, SessionPlan, get_in_progress_session, get_next_session_plan, get_session_plan,
};
use crate::api::settings::{Settings, get_settings};
use crate::ui::components::{Button, Card, EmptyState, LoadingState};
use crate::ui::errors::{BannerKind, Errors, use_errors};
use crate::ui::shell::Route;
use crate::ui::weight::{UnitSetting, use_unit};
use workout::Workout;

/// A session in progress, as the screen works on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Active {
    pub plan: SessionPlan,
    pub settings: Settings,
    /// The sets saved so far, in logging order.
    pub sets: Vec<LoggedSet<Timestamp>>,
    /// The exercises skipped on this device.
    pub skipped: BTreeSet<ExerciseId>,
}

/// What the page shows.
#[derive(Debug, Clone, PartialEq)]
enum Page {
    Loading,
    /// Loading failed (the banner says why).
    Failed,
    /// No session in progress: the next one, ready to start.
    Start(Box<NextSessionPlan>, Box<Settings>),
    /// No session can start (no active program): the server's message.
    Blocked(String),
    Active(Box<Active>),
}

/// The `/session` page.
#[component]
pub fn SessionPage() -> Element {
    let page = use_signal(|| Page::Loading);
    let errors = use_errors();
    let unit = use_context::<UnitSetting>();

    use_effect(move || {
        if cfg!(feature = "web") {
            spawn(load(page, errors, unit));
        }
    });

    let current = page.read().clone();
    match current {
        Page::Loading => rsx! { LoadingState { message: "Loading your workout…" } },
        Page::Failed => rsx! {
            EmptyState {
                title: "Workout not loaded",
                message: "Check your connection and try again.",
                Button { onclick: move |_| { spawn(load(page, errors, unit)); }, "Try again" }
            }
        },
        Page::Blocked(message) => rsx! {
            EmptyState { title: "No workout to start", message,
                Link { class: "io-button io-button-secondary", to: Route::Programs {}, "Choose a program" }
            }
        },
        Page::Start(next, settings) => rsx! {
            StartCard {
                next: *next,
                settings: *settings,
                on_started: move |active: Active| {
                    let mut page = page;
                    page.set(Page::Active(Box::new(active)));
                },
                on_reload: move |()| { spawn(load(page, errors, unit)); },
            }
        },
        Page::Active(active) => rsx! {
            Workout {
                key: "{active.plan.session.id}",
                initial: *active,
                on_reload: move |()| { spawn(load(page, errors, unit)); },
            }
        },
    }
}

/// Loads the settings, then the session in progress (with its plan) or the next one.
async fn load(mut page: Signal<Page>, errors: Errors, mut unit: UnitSetting) {
    page.set(Page::Loading);
    let settings = match get_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            errors.report(&error);
            page.set(Page::Failed);
            return;
        }
    };
    // The steppers and labels show weights in the user's unit.
    if *unit.0.peek() != settings.unit {
        unit.0.set(settings.unit);
    }
    let next = match get_in_progress_session().await {
        Ok(Some(in_progress)) => {
            let id = in_progress.session.id;
            match get_session_plan(id).await {
                Ok(plan) => Page::Active(Box::new(Active {
                    plan,
                    settings,
                    sets: in_progress.sets,
                    skipped: load_skipped(id),
                })),
                Err(error) => {
                    errors.report(&error);
                    Page::Failed
                }
            }
        }
        Ok(None) => match get_next_session_plan().await {
            Ok(next) => Page::Start(Box::new(next), Box::new(settings)),
            Err(error) => {
                let failure = ApiFailure::classify(&error);
                if failure.kind == FailureKind::Conflict {
                    // No active program: the page itself says so, with the way out.
                    Page::Blocked(failure.message)
                } else {
                    errors.report(&error);
                    Page::Failed
                }
            }
        },
        Err(error) => {
            errors.report(&error);
            Page::Failed
        }
    };
    page.set(next);
}

/// The `localStorage` key of the exercises skipped in a session.
fn skipped_key(session: SessionId) -> String {
    format!("io.session.{}.skipped", session.as_uuid())
}

/// The exercises skipped in `session` on this device.
fn load_skipped(session: SessionId) -> BTreeSet<ExerciseId> {
    platform::load(&skipped_key(session))
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Remembers the exercises skipped in `session`, so a reload does not bring them back.
fn store_skipped(session: SessionId, skipped: &BTreeSet<ExerciseId>) {
    if let Ok(json) = serde_json::to_string(skipped) {
        platform::store(&skipped_key(session), &json);
    }
}

/// Forgets what this device kept about `session`, once it has ended.
fn forget(session: SessionId) {
    platform::remove(&skipped_key(session));
    rest::clear(session);
}

/// The next workout, with its exercises, and the button that starts it.
#[component]
fn StartCard(
    next: NextSessionPlan,
    settings: Settings,
    on_started: EventHandler<Active>,
    on_reload: EventHandler<()>,
) -> Element {
    let errors = use_errors();
    let unit = use_unit();
    let mut busy = use_signal(|| false);
    // A start that failed is resent with the same id and time, so it is idempotent.
    let mut pending = use_signal(|| None::<(SessionId, Timestamp)>);
    let bar_weight = settings.bar_weight;

    let start = move |_| {
        // Inside the tap: lets iOS play the rest timer's beeps later.
        platform::unlock_audio();
        if *busy.peek() {
            return;
        }
        let (id, at) = pending
            .peek()
            .unwrap_or_else(|| (SessionId::new_v7(), platform::now()));
        pending.set(Some((id, at)));
        busy.set(true);
        let settings = settings.clone();
        spawn(async move {
            let started = writes::start_session(id, at).await;
            let plan = match started {
                Ok(session) => get_session_plan(session.id).await,
                Err(error) => Err(error),
            };
            busy.set(false);
            match plan {
                Ok(plan) => {
                    pending.set(None);
                    on_started.call(Active {
                        plan,
                        settings,
                        sets: Vec::new(),
                        skipped: BTreeSet::new(),
                    });
                }
                Err(error) => {
                    let failure = ApiFailure::classify(&error);
                    errors.report(&error);
                    if !failure.kind.is_retryable() {
                        pending.set(None);
                    }
                    if failure.kind == FailureKind::Conflict {
                        // Another session is in progress (another tab or device): resume it.
                        on_reload.call(());
                    }
                }
            }
        });
    };

    let lines: Vec<(String, String)> = next
        .exercises
        .iter()
        .map(|planned| {
            (
                planned.exercise.name.clone(),
                flow::exercise_summary(planned, bar_weight, unit),
            )
        })
        .collect();
    rsx! {
        div { class: "io-page-header",
            span { class: "io-label", "Next workout" }
            h1 { class: "io-title", "{next.day_name}" }
        }
        Card {
            ul { class: "io-list io-session-plan",
                for (index, (name, summary)) in lines.into_iter().enumerate() {
                    li { key: "{index}", class: "io-row",
                        div { class: "io-row-main",
                            span { class: "io-row-title", "{name}" }
                            span { class: "io-row-meta io-muted", "{summary}" }
                        }
                    }
                }
            }
        }
        Button { xl: true, block: true, busy: busy(), onclick: start, "Start workout" }
    }
}

/// Reports `message` as a note in the banner.
fn note(errors: Errors, message: &str) {
    errors.show(BannerKind::Info, message);
}
