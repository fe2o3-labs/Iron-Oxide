//! Inline SVG icons (24 × 24, stroked with `currentColor`), hidden from screen readers: the
//! control around them carries the name.

use dioxus::prelude::*;

#[component]
pub fn HomeIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            path { d: "M3 10.5 12 3l9 7.5" }
            path { d: "M5 9.5V21h14V9.5" }
            path { d: "M10 21v-6h4v6" }
        }
    }
}

#[component]
pub fn HistoryIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            circle { cx: "12", cy: "12", r: "9" }
            path { d: "M12 7v5l3 2" }
        }
    }
}

/// A barbell.
#[component]
pub fn ProgramsIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            path { d: "M2 12h20" }
            rect { x: "4", y: "7", width: "3", height: "10", rx: "1" }
            rect { x: "17", y: "7", width: "3", height: "10", rx: "1" }
        }
    }
}

#[component]
pub fn SettingsIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            path { d: "M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0" }
            circle { cx: "16", cy: "6", r: "2" }
            circle { cx: "10", cy: "12", r: "2" }
            circle { cx: "18", cy: "18", r: "2" }
        }
    }
}

/// A grip plate, for the plate calculator.
#[component]
pub fn PlateIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", "aria-hidden": "true",
            circle { cx: "12", cy: "12", r: "9" }
            circle { cx: "12", cy: "12", r: "2.5" }
        }
    }
}

#[component]
pub fn CloseIcon() -> Element {
    rsx! {
        svg { view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round", "aria-hidden": "true",
            path { d: "M6 6l12 12M18 6 6 18" }
        }
    }
}
