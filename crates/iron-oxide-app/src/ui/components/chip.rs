//! A chip: a small rounded tag (plates, filters). Clickable chips are 44 px high.

use dioxus::prelude::*;

/// A chip. With `onclick` it is a toggle button (`aria-pressed` from `selected`); without, a tag.
#[component]
pub fn Chip(
    selected: Option<bool>,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    match onclick {
        Some(handler) => rsx! {
            button {
                r#type: "button",
                class: "io-chip",
                aria_pressed: selected.unwrap_or(false).to_string(),
                onclick: move |event| handler.call(event),
                {children}
            }
        },
        None => rsx! {
            span { class: "io-chip", {children} }
        },
    }
}
