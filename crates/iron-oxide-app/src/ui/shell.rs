//! The app shell (#26): the routes, the mobile layout with its bottom navigation, and the sign-in
//! gate.
//!
//! Every page except the gallery needs an account, so the shell checks the session once (`me()`,
//! client only) and shows the sign-in screen while signed out. A `401` from any server call
//! (see [`crate::ui::errors`]) or signing out brings it back.

use dioxus::prelude::*;

use super::account::Account;
use super::components::icons::{HistoryIcon, HomeIcon, ProgramsIcon, SettingsIcon};
use super::components::{Card, EmptyState, LoadingState};
use super::errors::use_errors;
use crate::auth::api::{is_unauthorized, me};

/// The app's pages. Home, History, Programs and Settings are filled by their own tickets.
#[derive(Routable, Clone, PartialEq, Debug)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Shell)]
        #[route("/")]
        Home {},
        #[route("/history")]
        History {},
        #[route("/programs")]
        Programs {},
        #[route("/settings")]
        Settings {},
        #[route("/:..segments")]
        NotFound { segments: Vec<String> },
    #[end_layout]
    #[route("/dev/components")]
    Gallery {},
}

/// Whether the user is signed in, as far as the app knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Not known yet (server rendering, and until `me()` answers).
    Checking,
    SignedIn,
    SignedOut,
}

/// Provides the session status. Called once, by the app root.
pub fn use_session_provider() -> Signal<SessionStatus> {
    let status = use_signal(|| SessionStatus::Checking);
    use_context_provider(|| status)
}

/// The session status, to read or to update.
#[must_use]
pub fn use_session() -> Signal<SessionStatus> {
    use_context::<Signal<SessionStatus>>()
}

/// Sets the session status, unless it already is `status` (so that readers do not re-render).
pub fn set_session(mut session: Signal<SessionStatus>, status: SessionStatus) {
    if *session.peek() != status {
        session.set(status);
    }
}

/// The layout of every page: top bar, page, bottom navigation; or the sign-in screen.
#[component]
fn Shell() -> Element {
    let session = use_session();
    let errors = use_errors();

    // Client only: on the server the shell stays "Checking", so hydration matches.
    use_effect(move || {
        if cfg!(feature = "web") {
            spawn(async move {
                let status = match me().await {
                    Ok(_) => SessionStatus::SignedIn,
                    Err(error) if is_unauthorized(&error) => SessionStatus::SignedOut,
                    // Offline or a server problem: say so, and let the app work with what it has.
                    Err(error) => {
                        errors.report(&error);
                        SessionStatus::SignedIn
                    }
                };
                // A sign-in that finished meanwhile wins.
                if *session.peek() == SessionStatus::Checking {
                    set_session(session, status);
                }
            });
        }
    });

    let status = *session.read();
    match status {
        SessionStatus::Checking => rsx! {
            div { class: "io-shell io-shell-bare",
                TopBar {}
                LoadingState {}
            }
        },
        SessionStatus::SignedOut => rsx! {
            main { class: "io-shell io-shell-bare",
                TopBar {}
                div { class: "io-page-header",
                    h1 { class: "io-title", "Iron Oxide" }
                    p { class: "io-muted", "Zero-cost gains. The only overhead is the barbell." }
                }
                Account {}
            }
        },
        SessionStatus::SignedIn => rsx! {
            div { class: "io-shell",
                TopBar {}
                main { class: "io-page", Outlet::<Route> {} }
            }
            BottomNav {}
        },
    }
}

/// The top bar: the brand, and room for status indicators (offline, unsaved changes).
#[component]
fn TopBar() -> Element {
    rsx! {
        header { class: "io-topbar",
            span { class: "io-label", "Iron Oxide" }
            div { id: "io-status", class: "io-topbar-status" }
        }
    }
}

/// One entry of the bottom navigation.
struct NavItem {
    route: Route,
    label: &'static str,
    icon: fn() -> Element,
}

fn nav_items() -> [NavItem; 4] {
    [
        NavItem {
            route: Route::Home {},
            label: "Home",
            icon: || rsx! { HomeIcon {} },
        },
        NavItem {
            route: Route::History {},
            label: "History",
            icon: || rsx! { HistoryIcon {} },
        },
        NavItem {
            route: Route::Programs {},
            label: "Programs",
            icon: || rsx! { ProgramsIcon {} },
        },
        NavItem {
            route: Route::Settings {},
            label: "Settings",
            icon: || rsx! { SettingsIcon {} },
        },
    ]
}

/// The bottom navigation: four 64 px tabs, above the home indicator.
#[component]
fn BottomNav() -> Element {
    let current = use_route::<Route>();
    rsx! {
        nav { class: "io-nav", aria_label: "Main",
            ul {
                for item in nav_items() {
                    li { key: "{item.label}",
                        Link {
                            to: item.route.clone(),
                            aria_current: if item.route == current { "page" } else { "false" },
                            {(item.icon)()}
                            span { "{item.label}" }
                        }
                    }
                }
            }
        }
    }
}

/// A page's title.
#[component]
fn PageHeader(#[props(into)] title: String, #[props(into)] subtitle: Option<String>) -> Element {
    rsx! {
        div { class: "io-page-header",
            h1 { class: "io-title", "{title}" }
            if let Some(subtitle) = subtitle {
                p { class: "io-muted", "{subtitle}" }
            }
        }
    }
}

#[component]
fn Home() -> Element {
    rsx! {
        PageHeader { title: "Today" }
        EmptyState {
            title: "Nothing planned",
            message: "Your program's next workout will show up here.",
        }
    }
}

#[component]
fn History() -> Element {
    rsx! {
        PageHeader { title: "History" }
        EmptyState {
            title: "No workouts yet",
            message: "Finished workouts and your records will show up here.",
        }
    }
}

#[component]
fn Programs() -> Element {
    rsx! {
        PageHeader { title: "Programs" }
        EmptyState {
            title: "No program yet",
            message: "Pick a built-in program or upload your own here.",
        }
    }
}

#[component]
fn Settings() -> Element {
    rsx! {
        PageHeader { title: "Settings" }
        Account {}
        Card { title: "Units",
            p { class: "io-muted", "Weights are shown in kilograms." }
        }
    }
}

#[component]
fn NotFound(segments: Vec<String>) -> Element {
    let path = format!("/{}", segments.join("/"));
    rsx! {
        EmptyState {
            title: "Not found",
            message: "There is nothing at {path}.",
            Link { class: "io-button io-button-secondary", to: Route::Home {}, "Go home" }
        }
    }
}

/// The component gallery: debug builds only. Release builds answer with the not-found page.
#[component]
fn Gallery() -> Element {
    #[cfg(debug_assertions)]
    {
        rsx! { super::gallery::Gallery {} }
    }
    #[cfg(not(debug_assertions))]
    {
        rsx! {
            main { class: "io-shell io-shell-bare",
                NotFound { segments: vec!["dev".to_owned(), "components".to_owned()] }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_have_their_paths() {
        assert_eq!(Route::Home {}.to_string(), "/");
        assert_eq!(Route::History {}.to_string(), "/history");
        assert_eq!(Route::Programs {}.to_string(), "/programs");
        assert_eq!(Route::Settings {}.to_string(), "/settings");
        assert_eq!(Route::Gallery {}.to_string(), "/dev/components");
        assert_eq!("/history".parse::<Route>().unwrap(), Route::History {});
        assert_eq!(
            "/nope/x".parse::<Route>().unwrap(),
            Route::NotFound {
                segments: vec!["nope".to_owned(), "x".to_owned()]
            }
        );
    }

    #[test]
    fn the_bottom_navigation_has_the_four_sections() {
        let labels: Vec<_> = nav_items().iter().map(|item| item.label).collect();
        assert_eq!(labels, ["Home", "History", "Programs", "Settings"]);
    }
}
