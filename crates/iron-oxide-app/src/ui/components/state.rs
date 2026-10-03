//! Full-screen states: loading, and nothing to show yet.

use dioxus::prelude::*;

/// A page that is loading.
#[component]
pub fn LoadingState(#[props(into, default = "Loading…".to_owned())] message: String) -> Element {
    rsx! {
        div { class: "io-state", role: "status", aria_live: "polite",
            div { class: "io-spinner", aria_hidden: "true" }
            p { "{message}" }
        }
    }
}

/// A page with nothing to show yet, with an optional action as children.
#[component]
pub fn EmptyState(
    #[props(into)] title: String,
    #[props(into)] message: String,
    children: Element,
) -> Element {
    rsx! {
        div { class: "io-state",
            h2 { class: "io-title", "{title}" }
            p { "{message}" }
            {children}
        }
    }
}
