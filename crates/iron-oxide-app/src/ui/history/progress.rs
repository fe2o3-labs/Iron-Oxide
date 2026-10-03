//! An exercise's progress: its estimated 1RM and its top set over time, as inline SVG charts with
//! a data table. A Pro feature (`Feature::ExerciseCharts`): without it, the locked card.

use dioxus::prelude::*;
use iron_oxide_domain::{ExerciseId, Unit};

use super::chart::{self, VIEW_HEIGHT, VIEW_WIDTH, WeightPoint};
use super::view::{chart_series, chart_summary, latest_number, series_rows};
use super::{BackToHistory, ChartAccess, local_date, use_history};
use crate::api::error::{ApiFailure, FailureKind};
use crate::api::history::{ExerciseSeries, exercise_series};
use crate::ui::components::{Card, EmptyState, LoadingState};
use crate::ui::errors::use_errors;
use crate::ui::weight::use_unit;

/// The progress screen of `exercise`.
#[component]
pub fn ExerciseProgress(exercise: ExerciseId) -> Element {
    let history = use_history();
    let name = history.names.read().exercise(&exercise);
    let content = match *history.charts.read() {
        ChartAccess::Checking => rsx! { LoadingState {} },
        ChartAccess::Locked => rsx! { LockedCharts {} },
        ChartAccess::Unknown => rsx! {
            EmptyState {
                title: "Couldn't load",
                message: "Your plan could not be checked. Check your connection and try again.",
            }
        },
        ChartAccess::Included => rsx! { Charts { exercise: exercise.clone() } },
    };

    rsx! {
        BackToHistory {}
        div { class: "io-page-header",
            span { class: "io-label", "Progress" }
            h1 { class: "io-title", "{name}" }
        }
        {content}
    }
}

/// The card shown instead of the charts when the plan does not include them.
#[component]
pub fn LockedCharts() -> Element {
    rsx! {
        section { class: "io-card io-locked", aria_labelledby: "io-locked-title",
            div { class: "io-locked-head",
                LockIcon {}
                h2 { id: "io-locked-title", "Progress charts" }
                span { class: "io-chip", "Pro" }
            }
            p {
                "Charts of your top set and estimated 1RM over time, for every exercise, are part of Iron Oxide Pro."
            }
            p { class: "io-muted",
                "Your workouts and their sets stay in your history on every plan."
            }
        }
    }
}

#[component]
fn LockIcon() -> Element {
    rsx! {
        svg {
            class: "io-locked-icon",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            rect { x: "5", y: "11", width: "14", height: "10", rx: "2" }
            path { d: "M8 11V7a4 4 0 0 1 8 0v4" }
        }
    }
}

/// Loads the series and shows the charts.
#[component]
fn Charts(exercise: ExerciseId) -> Element {
    let errors = use_errors();
    let mut series = use_resource(use_reactive!(|exercise| async move {
        let result = exercise_series(exercise.as_str().to_owned()).await;
        if let Err(error) = &result {
            errors.report(error);
        }
        result
    }));

    match &*series.read() {
        None => rsx! { LoadingState { message: "Loading your progress…" } },
        // The plan changed since it was checked: the server refused the charts.
        Some(Err(error)) if ApiFailure::classify(error).kind == FailureKind::Forbidden => {
            rsx! { LockedCharts {} }
        }
        Some(Err(_)) => rsx! {
            EmptyState {
                title: "Couldn't load",
                message: "Your progress could not be loaded. Check your connection and try again.",
                button {
                    r#type: "button",
                    class: "io-button io-button-secondary",
                    onclick: move |_| series.restart(),
                    "Try again"
                }
            }
        },
        Some(Ok(loaded)) if loaded.points.is_empty() => rsx! {
            EmptyState {
                title: "No data yet",
                message: "Log a weighted working set of this exercise and finish the workout to start its charts.",
            }
        },
        Some(Ok(loaded)) => rsx! { Loaded { series: loaded.clone() } },
    }
}

/// The two charts and the table of a non-empty series.
#[component]
fn Loaded(series: ExerciseSeries) -> Element {
    let unit = use_unit();
    let lines = chart_series(&series);
    let rows = series_rows(&series, unit);

    rsx! {
        ChartCard {
            id: "e1rm",
            title: "Estimated 1RM",
            points: lines.e1rm,
            unit,
            empty: "No estimate yet: estimates need sets of 10 reps or fewer.",
        }
        ChartCard {
            id: "top-set",
            title: "Top set",
            points: lines.top_set,
            unit,
            empty: "No top set yet.",
        }
        Card { title: "Sessions",
            table { class: "io-table",
                caption { class: "io-sr-only", "Top set and estimated 1RM per session, newest first" }
                thead {
                    tr {
                        th { scope: "col", "Date" }
                        th { scope: "col", "Top set" }
                        th { scope: "col", "e1RM" }
                    }
                }
                tbody {
                    for row in rows {
                        tr { key: "{row.at_ms}-{row.top_set}",
                            th { scope: "row", {local_date(row.at_ms).short()} }
                            td { "{row.top_set}" }
                            td { "{row.e1rm}" }
                        }
                    }
                }
            }
        }
    }
}

/// One line chart in a card: the latest value big, the chart, and a sentence for screen readers.
#[component]
fn ChartCard(
    id: &'static str,
    title: &'static str,
    points: Vec<WeightPoint>,
    unit: Unit,
    empty: &'static str,
) -> Element {
    let date = |ms: i64| local_date(ms).short();
    let summary = chart_summary(title, &points, unit, date);
    let latest = latest_number(&points, unit);
    let layout = chart::layout(&points, unit, date);
    let title_id = format!("io-chart-{id}");

    rsx! {
        section { class: "io-card io-chart-card", aria_labelledby: "{title_id}",
            div { class: "io-chart-head",
                h2 { id: "{title_id}", "{title}" }
                if let Some(latest) = latest {
                    p { class: "io-chart-latest",
                        span { class: "io-sr-only", "Latest: " }
                        span { class: "io-chart-number", "{latest}" }
                        span { class: "io-chart-unit", "{unit.symbol()}" }
                    }
                }
            }
            match layout {
                None => rsx! { p { class: "io-muted", "{empty}" } },
                Some(layout) => rsx! {
                    svg {
                        class: "io-chart",
                        view_box: "0 0 {VIEW_WIDTH} {VIEW_HEIGHT}",
                        role: "img",
                        "aria-label": "{summary}",
                        g { class: "io-chart-grid", "aria-hidden": "true",
                            for tick in layout.y_ticks.iter() {
                                g { key: "{tick.label}",
                                line {
                                    x1: "{layout.plot.0}",
                                    x2: "{layout.plot.2}",
                                    y1: "{tick.at}",
                                    y2: "{tick.at}",
                                }
                                text {
                                    x: "{layout.plot.0 - 8.0}",
                                    y: "{tick.at}",
                                    text_anchor: "end",
                                    dominant_baseline: "middle",
                                    "{tick.label}"
                                }
                                }
                            }
                            for (index, tick) in layout.x_ticks.iter().enumerate() {
                                text {
                                    key: "x-{index}",
                                    x: "{tick.at}",
                                    y: "{layout.plot.3 + 20.0}",
                                    text_anchor: if index == 0 && layout.x_ticks.len() > 1 { "start" } else if index == 0 { "middle" } else { "end" },
                                    "{tick.label}"
                                }
                            }
                        }
                        g { class: "io-chart-series", "aria-hidden": "true",
                            path { class: "io-chart-line", d: "{layout.path}" }
                            for (index, dot) in layout.dots.iter().enumerate() {
                                circle {
                                    key: "{index}",
                                    class: "io-chart-dot",
                                    cx: "{dot.x}",
                                    cy: "{dot.y}",
                                    r: "3.5",
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}
