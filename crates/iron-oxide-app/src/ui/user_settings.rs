//! The user's settings, shared by every screen (#34), and the one place that saves them.
//!
//! [`use_user_settings`] gives the signed-in user's settings (bar, plates, rest, sound). They are
//! loaded as soon as the session is signed in, not when the Settings page opens, and they drive the
//! display unit of [`crate::ui::weight`], so every screen shows weights in the user's unit.
//!
//! Saving lives here, at the app root, not in the Settings page, so leaving the page never drops a
//! change:
//! - [`UserSettings::change`] applies a change on screen at once and queues it;
//! - one request is in flight at a time, and changes made meanwhile are coalesced into the latest,
//!   so a slow answer never overwrites a newer choice;
//! - a refused change is reported and rolled back everywhere;
//! - when the page is hidden or unloaded with a change not yet confirmed, the latest settings are
//!   sent once more with a `keepalive` request, which the browser completes after the page is gone.
//!
//! Everything is tied to the user who loaded it. Signing out (or the session ending) clears the
//! settings and bumps a generation: an answer that arrives afterwards, for a load or a save, is
//! dropped, so one user's settings can never be shown to, or saved for, the next.

use dioxus::prelude::*;
use iron_oxide_domain::Unit;

use super::errors::{Errors, use_errors};
use super::shell::{SessionStatus, use_session};
use super::weight::UnitSetting;
use crate::api::settings::{Settings, SettingsUpdate, get_settings, update_settings};
use crate::auth::api::me;
use crate::auth::types::UserId;

/// The signals behind [`UserSettings`].
#[derive(Clone, Copy, PartialEq)]
struct State {
    /// What the screens show: the saved settings with the changes being saved applied. `None`
    /// until loaded, and while signed out.
    current: Signal<Option<Settings>>,
    /// The settings as the server last confirmed them, to roll back to.
    confirmed: Signal<Option<Settings>>,
    /// The user the settings belong to.
    user: Signal<Option<UserId>>,
    /// Bumped when the user signs out: work started before is dropped when it finishes.
    generation: Signal<u64>,
    /// The generation being loaded, if a load is in flight.
    loading: Signal<Option<u64>>,
    /// Whether the last load failed (the banner says why).
    failed: Signal<bool>,
    session: Signal<SessionStatus>,
    unit: UnitSetting,
    errors: Errors,
}

/// The settings shared by every screen, provided by the app root. `Copy`.
#[derive(Clone, Copy, PartialEq)]
pub struct UserSettings {
    state: State,
    saver: Coroutine<(u64, Settings)>,
}

impl State {
    /// Shows `settings` (and their unit everywhere).
    fn show(self, settings: Option<Settings>) {
        let mut unit = self.unit.0;
        let wanted = settings.as_ref().map_or(Unit::Kg, |settings| settings.unit);
        if *unit.peek() != wanted {
            unit.set(wanted);
        }
        let mut current = self.current;
        current.set(settings);
    }

    /// Whether work started in `generation` still applies.
    fn is_current(self, generation: u64) -> bool {
        *self.generation.peek() == generation && *self.session.peek() != SessionStatus::SignedOut
    }

    async fn load(mut self) {
        let generation = *self.generation.peek();
        if *self.loading.peek() == Some(generation) {
            return;
        }
        self.loading.set(Some(generation));
        self.failed.set(false);
        let result = async {
            let user = me().await?.user_id;
            let settings = get_settings().await?;
            Ok::<_, ServerFnError>((user, settings))
        }
        .await;
        if !self.is_current(generation) {
            // Signed out meanwhile: these settings are not for whoever is here now.
            return;
        }
        self.loading.set(None);
        match result {
            Ok((user, settings)) => {
                self.user.set(Some(user));
                self.confirmed.set(Some(settings.clone()));
                self.show(Some(settings));
            }
            Err(error) => {
                self.errors.report(&error);
                self.failed.set(true);
            }
        }
    }

    /// Forgets everything (signed out).
    fn clear(mut self) {
        let next = *self.generation.peek() + 1;
        self.generation.set(next);
        self.loading.set(None);
        self.failed.set(false);
        self.user.set(None);
        self.confirmed.set(None);
        self.show(None);
        unsaved::set(None);
    }

    /// Saves changes one at a time, always the latest queued one.
    async fn save_loop(self, mut changes: UnboundedReceiver<(u64, Settings)>) {
        while let Ok(mut change) = changes.recv().await {
            while let Ok(newer) = changes.try_recv() {
                change = newer;
            }
            let (generation, wanted) = change;
            if !self.is_current(generation) {
                continue;
            }
            let result = update_settings(SettingsUpdate::from(wanted.clone())).await;
            if !self.is_current(generation) {
                continue;
            }
            let latest = self.current.peek().as_ref() == Some(&wanted);
            match result {
                Ok(saved) => {
                    let mut confirmed = self.confirmed;
                    confirmed.set(Some(saved.clone()));
                    if latest {
                        self.show(Some(saved));
                        unsaved::set(None);
                    }
                }
                Err(error) => {
                    self.errors.report(&error);
                    if latest {
                        self.show(self.confirmed.peek().clone());
                        unsaved::set(None);
                    }
                }
            }
        }
    }
}

impl UserSettings {
    /// The settings, if loaded.
    #[must_use]
    pub fn get(&self) -> Option<Settings> {
        self.state.current.read().clone()
    }

    /// The settings without subscribing to them (for event handlers).
    #[must_use]
    pub fn peek(&self) -> Option<Settings> {
        self.state.current.peek().clone()
    }

    /// The user the settings belong to, once loaded.
    #[must_use]
    pub fn user(&self) -> Option<UserId> {
        *self.state.user.read()
    }

    /// Whether loading failed; [`UserSettings::reload`] tries again.
    #[must_use]
    pub fn failed(&self) -> bool {
        *self.state.failed.read()
    }

    /// Loads the settings again.
    pub fn reload(self) {
        spawn(self.state.load());
    }

    /// Changes the settings with `edit`: on screen at once, then saved.
    pub fn change(self, edit: impl FnOnce(Settings) -> Settings) {
        let Some(settings) = self.peek() else {
            return;
        };
        let settings = edit(settings);
        self.state.show(Some(settings.clone()));
        unsaved::set(Some(&settings));
        self.saver.send((*self.state.generation.peek(), settings));
    }
}

/// Provides the shared settings, loads them whenever the session becomes signed in, and saves
/// changes. Called once, by the app root, after the session, the banner and the unit.
pub fn use_settings_provider(unit: UnitSetting) -> UserSettings {
    let state = State {
        current: use_signal(|| None),
        confirmed: use_signal(|| None),
        user: use_signal(|| None),
        generation: use_signal(|| 0),
        loading: use_signal(|| None),
        failed: use_signal(|| false),
        session: use_session(),
        unit,
        errors: use_errors(),
    };
    let saver = use_coroutine(move |changes| state.save_loop(changes));
    let settings = use_context_provider(|| UserSettings { state, saver });
    use_hook(unsaved::flush_when_hidden);
    // Client only: the server renders the signed-out shell, so hydration matches.
    use_effect(move || {
        if !cfg!(feature = "web") {
            return;
        }
        match *state.session.read() {
            SessionStatus::SignedIn => {
                spawn(state.load());
            }
            SessionStatus::SignedOut => state.clear(),
            SessionStatus::Checking | SessionStatus::Unverified => {}
        }
    });
    settings
}

/// The shared settings.
#[must_use]
pub fn use_user_settings() -> UserSettings {
    use_context::<UserSettings>()
}

/// The latest settings not yet confirmed by the server, as the request body that saves them, for
/// the last-chance save when the page is hidden or closed. Kept outside the Dioxus runtime, since
/// the browser calls the listeners outside it.
mod unsaved {
    use std::cell::RefCell;

    use crate::api::settings::{Settings, SettingsUpdate};

    /// The server function's route (see `crate::api::settings::update_settings`).
    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub const UPDATE_PATH: &str = "/api/settings/update";

    thread_local! {
        static BODY: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    /// The JSON body of `update_settings(settings)`.
    #[must_use]
    pub fn body(settings: &Settings) -> String {
        serde_json::json!({ "settings": SettingsUpdate::from(settings.clone()) }).to_string()
    }

    pub fn set(settings: Option<&Settings>) {
        BODY.with(|cell| *cell.borrow_mut() = settings.map(body));
    }

    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub fn get() -> Option<String> {
        BODY.with(|cell| cell.borrow().clone())
    }

    /// Installs the listeners that send the unsaved settings when the page is hidden (the user
    /// switches app, locks the phone) or unloaded. A full replace, so sending it twice is harmless.
    #[cfg(feature = "web")]
    pub fn flush_when_hidden() {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        let Some(window) = web_sys::window() else {
            return;
        };
        let flush = Closure::<dyn Fn()>::new(|| {
            let hidden = web_sys::window()
                .and_then(|window| window.document())
                .is_none_or(|document| document.hidden());
            if hidden && let Some(body) = get() {
                send_keepalive(&body);
            }
        });
        let callback = flush.as_ref().unchecked_ref();
        let _ = window.add_event_listener_with_callback("pagehide", callback);
        if let Some(document) = window.document() {
            let _ = document.add_event_listener_with_callback("visibilitychange", callback);
        }
        // Lives as long as the page.
        flush.forget();
    }

    #[cfg(not(feature = "web"))]
    pub const fn flush_when_hidden() {}

    /// A `POST` the browser completes even if the page goes away (`keepalive`).
    #[cfg(feature = "web")]
    fn send_keepalive(body: &str) {
        use wasm_bindgen::JsValue;

        let Some(window) = web_sys::window() else {
            return;
        };
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&JsValue::from_str(body));
        let headers = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &headers,
            &JsValue::from_str("content-type"),
            &JsValue::from_str("application/json"),
        );
        init.set_headers(&headers);
        let _ = js_sys::Reflect::set(&init, &JsValue::from_str("keepalive"), &JsValue::TRUE);
        // Best effort: the page is going away, nobody can show an error.
        let _ = window.fetch_with_str_and_init(UPDATE_PATH, &init);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_body_is_the_server_functions_arguments() {
            let settings = Settings::defaults();
            let body: serde_json::Value = serde_json::from_str(&body(&settings)).unwrap();
            let update: SettingsUpdate = serde_json::from_value(body["settings"].clone()).unwrap();
            assert_eq!(update, SettingsUpdate::from(settings.clone()));
            set(Some(&settings));
            assert!(get().is_some());
            set(None);
            assert_eq!(get(), None);
        }
    }
}
