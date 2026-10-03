//! The plate calculator's slot on the set screen (#31 fills it).

use dioxus::prelude::*;
use iron_oxide_domain::Weight;

use super::sheet::Sheet;
use crate::ui::components::{Button, ButtonVariant};
use crate::ui::weight::{use_unit, weight_text};

/// Opened by the "Plate calculator" button of the set screen, for the weight on the stepper.
/// A placeholder until the plate calculator (#31) lands.
#[component]
pub fn PlateCalculator(weight: Weight, on_close: EventHandler<()>) -> Element {
    let unit = use_unit();
    let target = weight_text(weight, unit);
    rsx! {
        Sheet { title: "Plate calculator", on_close,
            p { class: "io-muted", "Loading {target} on the bar: the plate calculator is coming soon." }
            Button { variant: ButtonVariant::Secondary, block: true, onclick: move |_| on_close.call(()), "Close" }
        }
    }
}
