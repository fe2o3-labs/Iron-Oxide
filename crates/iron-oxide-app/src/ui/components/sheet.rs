//! A bottom sheet: a panel over the page for a short side task (the plate calculator), closed with
//! its close button, Escape or a tap outside it.

use dioxus::prelude::*;

use super::IconButton;
use super::icons::CloseIcon;

/// A modal bottom sheet titled `title`. It takes the focus when it opens, so Escape closes it at
/// once; the close button, Escape and a tap on the backdrop call `on_close`. It sits above the
/// bottom navigation and below the error banner.
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
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    on_close.call(());
                }
            },
            div {
                class: "io-sheet",
                role: "dialog",
                aria_modal: "true",
                aria_label: "{title}",
                tabindex: "-1",
                onclick: move |event| event.stop_propagation(),
                onmounted: move |event| async move {
                    // Best effort: without focus, the close button and the backdrop still work.
                    let _ = event.set_focus(true).await;
                },
                div { class: "io-sheet-header",
                    h2 { class: "io-label", "{title}" }
                    IconButton { label: "Close", onclick: move |_| on_close.call(()), CloseIcon {} }
                }
                {children}
            }
        }
    }
}
