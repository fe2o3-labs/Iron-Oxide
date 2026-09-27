//! User interface components.

use dioxus::prelude::*;

use crate::api::server_time;

/// Root component: a hello-world page with one server function round-trip.
#[component]
pub fn App() -> Element {
    let mut time = use_action(server_time);

    rsx! {
        document::Title { "Iron Oxide" }
        main {
            h1 { "Iron Oxide" }
            p { "Zero-cost gains. The only overhead is the barbell." }
            button {
                id: "server-time",
                disabled: time.pending(),
                onclick: move |_| {
                    time.call();
                },
                "Ask the server for the time"
            }
            match time.value() {
                None if time.pending() => rsx! { p { "Asking the server…" } },
                None => rsx! {},
                Some(Ok(seconds)) => rsx! { p { id: "server-time-result", "Server time: {seconds} seconds since the Unix epoch" } },
                Some(Err(error)) => rsx! { p { id: "server-time-error", role: "alert", "Could not reach the server: {error}" } },
            }
        }
    }
}
