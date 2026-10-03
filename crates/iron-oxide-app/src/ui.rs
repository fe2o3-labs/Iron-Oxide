//! User interface: the app root, the shell and its pages, and the shared components.
//!
//! - `theme`: the stylesheet and the fonts.
//! - `shell`: the routes, the layout with the bottom navigation, and the sign-in gate.
//! - `components`: the reusable components.
//! - `errors`: the banner every server error is reported to.
//! - `weight`: weights in the user's unit.
//! - `home`: the home screen.
//! - `session`: the workout session; its writes go through `session::writes`.
//! - `history`: the history screens (#33).

mod account;
#[cfg_attr(
    not(debug_assertions),
    allow(
        dead_code,
        unused_imports,
        reason = "used by the screens of #27-#34; until then only by the debug gallery"
    )
)]
mod components;
#[cfg_attr(
    not(debug_assertions),
    allow(
        dead_code,
        unused_imports,
        reason = "used by the screens of #27-#34; until then only by the debug gallery"
    )
)]
mod errors;
#[cfg(debug_assertions)]
mod gallery;
mod home;
mod session;
mod history;
mod shell;
pub(crate) mod theme;
pub mod unsaved;
#[cfg_attr(
    not(debug_assertions),
    allow(
        dead_code,
        unused_imports,
        reason = "used by the screens of #27-#34; until then only by the debug gallery"
    )
)]
mod weight;

use dioxus::prelude::*;

use crate::pwa::PwaHead;
use components::BannerHost;
use shell::Route;

/// Root component: the head, the shared state, the routes and the banner.
#[component]
pub fn App() -> Element {
    shell::use_session_provider();
    errors::use_errors_provider();
    weight::use_unit_provider();
    crate::offline::use_outbox_provider();

    rsx! {
        document::Title { "Iron Oxide" }
        PwaHead {}
        document::Meta {
            name: "viewport",
            content: "width=device-width, initial-scale=1, viewport-fit=cover",
        }
        for url in theme::FONT_URLS {
            document::Link {
                rel: "preload",
                href: url,
                r#as: "font",
                r#type: "font/woff2",
                crossorigin: "anonymous",
            }
        }
        document::Stylesheet { href: theme::APP_CSS }
        Router::<Route> {}
        BannerHost {}
        unsaved::Unsaved {}
    }
}
