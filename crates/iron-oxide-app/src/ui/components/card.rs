//! A card: a rounded surface grouping one thing.

use dioxus::prelude::*;

/// A card. With a `title`, it is a labelled section with an `h2`.
#[component]
pub fn Card(#[props(into)] title: Option<String>, children: Element) -> Element {
    rsx! {
        section { class: "io-card",
            if let Some(title) = title {
                h2 { "{title}" }
            }
            {children}
        }
    }
}
