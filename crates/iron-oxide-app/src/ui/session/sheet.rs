//! A bottom sheet over the session screen, for confirmations and the plate calculator.

use dioxus::prelude::*;

/// A modal sheet with a title; `children` hold its text and buttons. Tapping the backdrop or
/// pressing Escape calls `on_close`.
#[component]
pub fn Sheet(
    #[props(into)] title: String,
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        div {
            class: "io-sheet-backdrop",
            onclick: move |_| on_close.call(()),
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    on_close.call(());
                }
            },
            div {
                class: "io-sheet",
                role: "dialog",
                aria_modal: "true",
                aria_labelledby: "io-sheet-title",
                onclick: move |event| event.stop_propagation(),
                h2 { id: "io-sheet-title", "{title}" }
                {children}
            }
        }
    }
}
