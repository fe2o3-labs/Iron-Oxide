//! The app shell (#26): the routes, the mobile layout with its bottom navigation, and the sign-in
//! gate.
//!
//! Every page except the gallery needs an account, so the shell checks the session (`me()`,
//! client only) and shows the sign-in screen while signed out. A `401` from any server call
//! (see [`crate::ui::errors`]) or signing out brings it back.
//!
//! Offline first, deliberately: when `me()` cannot answer (offline, `429`, `503`), the app still
//! opens, so it works at the gym without signal. A banner says so, and `me()` is retried with a
//! backoff until the server answers; a `401` then shows the sign-in screen.

use dioxus::prelude::*;

use super::account::Account;
use super::components::icons::{HistoryIcon, HomeIcon, ProgramsIcon, SettingsIcon};
use super::components::{Card, EmptyState, LoadingState};
use super::errors::{BannerKind, use_errors};
use crate::api::error::{ApiFailure, FailureKind};
use crate::auth::api::{is_unauthorized, me};
use crate::auth::browser;

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
    /// `me()` could not answer (offline, `429`, `503`): the app is shown with what is on this
    /// device while the check is retried.
    Unverified,
}

/// The longest wait between two session checks.
const MAX_RETRY_SECS: u64 = 60;

/// How long to wait before session check number `attempt + 1`: 2, 4, 8… seconds up to a minute,
/// and never less than a `429`'s `Retry-After`.
#[must_use]
pub fn retry_delay_secs(attempt: u32, retry_after: Option<u64>) -> u64 {
    let backoff = 2_u64
        .saturating_pow(attempt.saturating_add(1))
        .min(MAX_RETRY_SECS);
    backoff.max(retry_after.unwrap_or(0))
}

/// The banner while the session cannot be checked.
#[must_use]
pub const fn unverified_message(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Network => "Offline \u{2014} showing what's on this device.",
        _ => "Can't reach the server, retrying. Showing what's on this device.",
    }
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
            spawn(check_session(session, errors));
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
                div { class: "io-page-header",
                    h1 { class: "io-title", "Iron Oxide" }
                    p { class: "io-muted", "Zero-cost gains. The only overhead is the barbell." }
                }
                Account {}
            }
        },
        SessionStatus::SignedIn | SessionStatus::Unverified => rsx! {
            div { class: "io-shell",
                TopBar {}
                main { class: "io-page", Outlet::<Route> {} }
            }
            BottomNav {}
        },
    }
}

/// Checks the session with `me()`, retrying with a backoff while the server cannot answer (see
/// the module docs). Stops as soon as the status is known, here or elsewhere (a sign-in, a `401`).
async fn check_session(session: Signal<SessionStatus>, errors: super::errors::Errors) {
    let mut attempt = 0_u32;
    let mut banner = None;
    loop {
        let status = match me().await {
            Ok(_) => SessionStatus::SignedIn,
            Err(error) if is_unauthorized(&error) => SessionStatus::SignedOut,
            Err(error) => {
                let failure = ApiFailure::classify(&error);
                if matches!(
                    *session.peek(),
                    SessionStatus::Checking | SessionStatus::Unverified
                ) {
                    set_session(session, SessionStatus::Unverified);
                    banner =
                        Some(errors.show(BannerKind::Warning, unverified_message(failure.kind)));
                } else {
                    // Known meanwhile (signed in or out elsewhere).
                    return;
                }
                let secs = retry_delay_secs(attempt, failure.retry_after_secs());
                browser::sleep(i32::try_from(secs * 1_000).unwrap_or(i32::MAX)).await;
                attempt = attempt.saturating_add(1);
                continue;
            }
        };
        if matches!(
            *session.peek(),
            SessionStatus::Checking | SessionStatus::Unverified
        ) {
            set_session(session, status);
        }
        if let Some(id) = banner {
            errors.dismiss_if(id);
        }
        return;
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
    fn session_checks_back_off_up_to_a_minute() {
        let delays: Vec<_> = (0..7)
            .map(|attempt| retry_delay_secs(attempt, None))
            .collect();
        assert_eq!(delays, [2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(retry_delay_secs(u32::MAX, None), 60);
        // A 429's Retry-After is honoured.
        assert_eq!(retry_delay_secs(0, Some(42)), 42);
        assert_eq!(retry_delay_secs(5, Some(10)), 60);
    }

    #[test]
    fn the_unverified_banner_says_why() {
        assert!(unverified_message(FailureKind::Network).starts_with("Offline"));
        for kind in [
            FailureKind::RateLimited,
            FailureKind::Transient,
            FailureKind::Other,
        ] {
            assert!(unverified_message(kind).starts_with("Can't reach the server, retrying"));
        }
    }

    #[test]
    fn the_bottom_navigation_has_the_four_sections() {
        let labels: Vec<_> = nav_items().iter().map(|item| item.label).collect();
        assert_eq!(labels, ["Home", "History", "Programs", "Settings"]);
    }
}
