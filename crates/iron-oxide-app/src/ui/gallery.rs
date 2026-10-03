//! The component gallery at `/dev/components`, in debug builds only: every component, in the dark
//! theme and then in the light one, for reviews and screenshots.

use dioxus::CapturedError;
use dioxus::fullstack::RequestError;
use dioxus::prelude::*;
use iron_oxide_domain::Weight;

use super::components::icons::PlateIcon;
use super::components::{
    Button, ButtonVariant, Card, Chip, EmptyState, IconButton, LoadingState, ProgressSegments,
    Stepper, WeightStepper,
};
use super::errors::{BannerKind, use_errors};
use super::shell::unverified_message;
use super::weight::{use_unit, weight_text};
use crate::api::error::FailureKind;

#[component]
pub fn Gallery() -> Element {
    rsx! {
        main { class: "io-shell io-shell-bare",
            h1 { class: "io-title", "Components" }
            p { class: "io-muted", "Debug builds only. Dark theme first, then light." }
            for theme in ["dark", "light"] {
                section { key: "{theme}", class: "io-gallery-theme", "data-theme": theme,
                    span { class: "io-label", "Theme · {theme}" }
                    Showcase {}
                }
            }
        }
    }
}

/// One copy of every component, with live state.
#[component]
fn Showcase() -> Element {
    let errors = use_errors();
    let unit = use_unit();
    let mut reps = use_signal(|| 5_i64);
    let mut weight = use_signal(|| Weight::from_kg(100.0).unwrap_or_default());
    let mut plate = use_signal(|| 2_usize);
    let mut sets_done = use_signal(|| 2_u32);
    let target = weight_text(*weight.read(), unit);

    rsx! {
        div { class: "io-topbar",
            span { class: "io-label", "Day A · Set {sets_done} / 5" }
            IconButton { label: "Plate calculator", PlateIcon {} }
        }
        div { class: "io-page-header",
            h2 { class: "io-title", style: "font-size: 64px", "Back squat" }
            p { class: "io-muted", "Target {reps} reps · {target} · rest 3:00" }
        }
        ProgressSegments { done: *sets_done.read(), total: 5, label: "Sets done" }

        Stepper {
            label: "Reps",
            value: *reps.read(),
            min: 0,
            max: 100,
            less_label: "One rep less",
            more_label: "One rep more",
            on_change: move |value| reps.set(value),
        }
        WeightStepper {
            value: *weight.read(),
            step: Weight::from_kg(2.5).unwrap_or_default(),
            on_change: move |value| weight.set(value),
        }
        div { class: "io-chips",
            span { class: "io-muted io-hint", "Per side" }
            for label in ["20", "15", "5", "2.5"] {
                Chip { key: "{label}", "{label}" }
            }
        }
        div { class: "io-chips",
            for (index, label) in ["1.25", "2.5", "5"].into_iter().enumerate() {
                Chip {
                    key: "{label}",
                    selected: *plate.read() == index,
                    onclick: move |_| plate.set(index),
                    "{label} kg"
                }
            }
        }

        Button {
            xl: true,
            block: true,
            onclick: move |_| {
                let next = (*sets_done.peek() + 1).min(5);
                sets_done.set(next);
            },
            "Done"
        }
        div { class: "io-actions",
            Button { variant: ButtonVariant::Secondary, block: true, "Secondary" }
            Button { variant: ButtonVariant::Ghost, block: true, "Ghost" }
            Button { variant: ButtonVariant::Danger, block: true, "Danger" }
            Button { block: true, busy: true, "Busy" }
            Button { variant: ButtonVariant::Secondary, block: true, disabled: true, "Disabled" }
        }

        Card { title: "Card",
            p { class: "io-muted", "A surface grouping one thing." }
        }

        span { class: "io-label", "Banners" }
        div { class: "io-actions",
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| {
                    errors.report(&ServerFnError::ServerError {
                        // As our rate limiter sends it (server/rate_limit.rs).
                        message: "Too many requests. Please try again in 42 seconds.".to_owned(),
                        code: 429,
                        details: Some(serde_json::json!({ "retry_after_secs": 42 })),
                    });
                },
                "Show a 429"
            }
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| {
                    errors.report_captured(&CapturedError::from(ServerFnError::Request(
                        RequestError::Connect("offline".to_owned()),
                    )));
                },
                "Show a network error"
            }
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| {
                    errors.show(BannerKind::Info, "Passkey added.");
                },
                "Show a note"
            }
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| {
                    errors.show(
                        BannerKind::Warning,
                        unverified_message(FailureKind::Network),
                    );
                },
                "Show offline"
            }
        }
        div { class: "io-banner", role: "presentation",
            p { "Cannot reach the server. Check your connection." }
        }

        Card {
            LoadingState { message: "Loading your program…" }
        }
        Card {
            EmptyState { title: "No workouts yet", message: "Finished workouts will show up here.",
                Button { variant: ButtonVariant::Secondary, "Pick a program" }
            }
        }
    }
}
