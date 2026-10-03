//! Progress through a fixed number of steps: one segment per set.

use dioxus::prelude::*;

/// `done` of `total` segments filled, e.g. set 2 of 5. `label` names the progress for screen
/// readers ("Sets done").
#[component]
pub fn ProgressSegments(done: u32, total: u32, #[props(into)] label: String) -> Element {
    let done = done.min(total);
    rsx! {
        div {
            class: "io-progress",
            role: "progressbar",
            aria_label: label,
            aria_valuemin: "0",
            aria_valuemax: "{total}",
            aria_valuenow: "{done}",
            aria_valuetext: "{done} of {total}",
            for index in 0..total {
                div {
                    key: "{index}",
                    class: "io-progress-segment",
                    "data-done": index < done,
                }
            }
        }
    }
}
