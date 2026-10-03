//! The `−` / value / `+` stepper: big numerals between two 72 px buttons, so a value is set with
//! the thumb instead of the keyboard.

use dioxus::prelude::*;
use iron_oxide_domain::Weight;

use crate::ui::weight::{Direction, step_weight, unit_name, use_unit, weight_number};

/// `value` moved one `step` in `direction`, kept within `min..=max`, without overflowing.
#[must_use]
pub fn step_count(value: i64, step: i64, direction: Direction, min: i64, max: i64) -> i64 {
    let step = step.max(1);
    let moved = match direction {
        Direction::Up => value.saturating_add(step),
        Direction::Down => value.saturating_sub(step),
    };
    moved.clamp(min, max.max(min))
}

/// The size of the numerals, in px: 96 px as designed, smaller for long values so that `1102.5`
/// still fits between the buttons on a 360 px wide phone.
#[must_use]
pub fn numeral_size(text: &str) -> u32 {
    match text.chars().count() {
        0..=3 => 96,
        4 => 84,
        5 => 68,
        _ => 56,
    }
}

/// A stepper over whole numbers (reps, seconds). `label` is shown under the value (`REPS`) and
/// names the group; `less_label` and `more_label` name the buttons for screen readers. While
/// `disabled`, both buttons are.
#[component]
pub fn Stepper(
    #[props(into)] label: String,
    value: i64,
    #[props(default = 1)] step: i64,
    #[props(default = 0)] min: i64,
    #[props(default = i64::MAX)] max: i64,
    on_change: EventHandler<i64>,
    #[props(into)] less_label: Option<String>,
    #[props(into)] more_label: Option<String>,
    #[props(default)] disabled: bool,
) -> Element {
    let less_label = less_label.unwrap_or_else(|| format!("{step} less"));
    let more_label = more_label.unwrap_or_else(|| format!("{step} more"));
    rsx! {
        StepperView {
            label,
            text: value.to_string(),
            less_label,
            more_label,
            can_decrease: !disabled && value > min,
            can_increase: !disabled && value < max,
            on_decrease: move |()| on_change.call(step_count(value, step, Direction::Down, min, max)),
            on_increase: move |()| on_change.call(step_count(value, step, Direction::Up, min, max)),
        }
    }
}

/// A stepper over a weight, shown in the user's unit. The steps are exact (see
/// [`step_weight`]); the label is the unit symbol (`KG`). While `disabled`, both buttons are.
#[component]
pub fn WeightStepper(
    value: Weight,
    step: Weight,
    #[props(default = Weight::ZERO)] min: Weight,
    #[props(default = Weight::MAX)] max: Weight,
    on_change: EventHandler<Weight>,
    #[props(default)] disabled: bool,
) -> Element {
    let unit = use_unit();
    let amount = format!("{} {}", weight_number(step, unit), unit_name(unit));
    rsx! {
        StepperView {
            label: unit.symbol(),
            text: weight_number(value, unit),
            less_label: "{amount} less",
            more_label: "{amount} more",
            can_decrease: !disabled && value > min,
            can_increase: !disabled && value < max,
            on_decrease: move |()| on_change.call(step_weight(value, step, Direction::Down, min, max)),
            on_increase: move |()| on_change.call(step_weight(value, step, Direction::Up, min, max)),
        }
    }
}

#[component]
fn StepperView(
    #[props(into)] label: String,
    text: String,
    #[props(into)] less_label: String,
    #[props(into)] more_label: String,
    can_decrease: bool,
    can_increase: bool,
    on_decrease: EventHandler<()>,
    on_increase: EventHandler<()>,
) -> Element {
    let size = numeral_size(&text);
    rsx! {
        div { class: "io-card io-stepper", role: "group", aria_label: "{label}",
            button {
                r#type: "button",
                class: "io-stepper-button io-stepper-minus",
                aria_label: less_label,
                disabled: !can_decrease,
                onclick: move |_| on_decrease.call(()),
                "−"
            }
            div { class: "io-stepper-value",
                output { class: "io-stepper-number", style: "font-size: {size}px", aria_live: "polite", "{text}" }
                span { class: "io-stepper-label", aria_hidden: "true", "{label}" }
            }
            button {
                r#type: "button",
                class: "io-stepper-button io-stepper-plus",
                aria_label: more_label,
                disabled: !can_increase,
                onclick: move |_| on_increase.call(()),
                "+"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_step_and_stay_in_range() {
        assert_eq!(step_count(5, 1, Direction::Up, 0, 10), 6);
        assert_eq!(step_count(5, 1, Direction::Down, 0, 10), 4);
        assert_eq!(step_count(0, 1, Direction::Down, 0, 10), 0);
        assert_eq!(step_count(10, 1, Direction::Up, 0, 10), 10);
        assert_eq!(step_count(170, 15, Direction::Up, 0, 180), 180);
        assert_eq!(
            step_count(i64::MAX, 1, Direction::Up, 0, i64::MAX),
            i64::MAX
        );
        // A step below 1 still moves.
        assert_eq!(step_count(5, 0, Direction::Up, 0, 10), 6);
    }

    #[test]
    fn long_values_get_smaller_numerals() {
        assert_eq!(numeral_size("5"), 96);
        assert_eq!(numeral_size("100"), 96);
        assert_eq!(numeral_size("2:30"), 84);
        assert_eq!(numeral_size("102.5"), 68);
        assert_eq!(numeral_size("1102.5"), 56);
    }
}
