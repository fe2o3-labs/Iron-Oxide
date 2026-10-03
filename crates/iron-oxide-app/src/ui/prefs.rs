//! Preferences of this device only (#34): the weight step of the steppers and vibration.
//!
//! The server's settings (`crate::api::settings`) have no field for them, so they live in the
//! browser's `localStorage`, under a key per user, and do not follow the user to another device.
//! Signing out removes them. A missing, unreadable or outdated entry gives the defaults; nothing
//! here can fail.

use dioxus::prelude::*;
use iron_oxide_domain::{Unit, Weight};
use serde::{Deserialize, Serialize};

use super::user_settings::UserSettings;
use crate::auth::types::UserId;

/// The start of the `localStorage` keys, one per user (`….<user id>`). Bump the version if the
/// shape changes incompatibly.
const STORAGE_KEY_PREFIX: &str = "iron-oxide.device-prefs.v1";

/// The weight steps offered, per unit, lightest first.
#[must_use]
pub const fn step_choices(unit: Unit) -> &'static [f64] {
    match unit {
        Unit::Kg => &[0.5, 1.0, 1.25, 2.5, 5.0],
        Unit::Lb => &[1.0, 2.5, 5.0, 10.0],
    }
}

/// The step a weight stepper moves by when the user never chose one: 2.5 kg or 5 lb.
#[must_use]
pub fn default_step(unit: Unit) -> Weight {
    let value = match unit {
        Unit::Kg => 2.5,
        Unit::Lb => 5.0,
    };
    Weight::new(value, unit).unwrap_or(Weight::ZERO)
}

/// This device's preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePrefs {
    /// The weight step in kg mode; `None` for [`default_step`].
    #[serde(default)]
    pub kg_step: Option<Weight>,
    /// The weight step in lb mode; `None` for [`default_step`].
    #[serde(default)]
    pub lb_step: Option<Weight>,
    /// Whether the rest timer vibrates the phone when it ends (where the browser can).
    #[serde(default = "yes")]
    pub vibration: bool,
}

const fn yes() -> bool {
    true
}

impl Default for DevicePrefs {
    fn default() -> Self {
        Self {
            kg_step: None,
            lb_step: None,
            vibration: true,
        }
    }
}

impl DevicePrefs {
    /// The step weight steppers move by in `unit`.
    #[must_use]
    pub fn weight_step(&self, unit: Unit) -> Weight {
        let chosen = match unit {
            Unit::Kg => self.kg_step,
            Unit::Lb => self.lb_step,
        };
        chosen
            .filter(|step| !step.is_zero())
            .unwrap_or_else(|| default_step(unit))
    }

    /// The same preferences with `step` as the weight step of `unit`.
    #[must_use]
    pub const fn with_weight_step(mut self, unit: Unit, step: Weight) -> Self {
        match unit {
            Unit::Kg => self.kg_step = Some(step),
            Unit::Lb => self.lb_step = Some(step),
        }
        self
    }

    /// Reads stored preferences; anything unreadable gives the defaults.
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Self {
        stored
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default()
    }

    /// The text stored for these preferences.
    #[must_use]
    pub fn to_stored(self) -> String {
        serde_json::to_string(&self).unwrap_or_default()
    }
}

/// The storage key of `user`'s preferences on this device.
#[must_use]
pub fn storage_key(user: UserId) -> String {
    format!("{STORAGE_KEY_PREFIX}.{user}")
}

/// This device's preferences for the signed-in user. `Copy`.
#[derive(Clone, Copy, PartialEq)]
pub struct DevicePrefsHandle {
    prefs: Signal<DevicePrefs>,
    user: Signal<Option<UserId>>,
}

impl DevicePrefsHandle {
    /// The preferences (the defaults while signed out).
    #[must_use]
    pub fn get(&self) -> DevicePrefs {
        *self.prefs.read()
    }

    /// The preferences without subscribing (for event handlers).
    #[must_use]
    pub fn peek(&self) -> DevicePrefs {
        *self.prefs.peek()
    }

    /// Changes the preferences and stores them for the signed-in user.
    pub fn save(self, new: DevicePrefs) {
        let mut prefs = self.prefs;
        prefs.set(new);
        if let Some(user) = *self.user.peek() {
            storage::write(&storage_key(user), &new.to_stored());
        }
    }
}

/// Provides this device's preferences, per user: loaded when the user's settings are (that is when
/// the app knows who is signed in), back to the defaults and removed from this device on sign-out.
/// Called once, by the app root, after the settings.
pub fn use_device_prefs_provider(settings: UserSettings) -> DevicePrefsHandle {
    let handle = DevicePrefsHandle {
        prefs: use_signal(DevicePrefs::default),
        user: use_signal(|| None),
    };
    use_context_provider(|| handle);
    // Client only, so the server-rendered page and hydration match.
    use_effect(move || {
        if !cfg!(feature = "web") {
            return;
        }
        let user = settings.user();
        let previous = *handle.user.peek();
        if user == previous {
            return;
        }
        let (mut prefs, mut current) = (handle.prefs, handle.user);
        if let (None, Some(previous)) = (user, previous) {
            // Signed out: nothing of theirs stays on this device.
            storage::remove(&storage_key(previous));
        }
        current.set(user);
        prefs.set(match user {
            Some(user) => DevicePrefs::parse(storage::read(&storage_key(user)).as_deref()),
            None => DevicePrefs::default(),
        });
    });
    handle
}

/// This device's preferences for the signed-in user.
#[must_use]
pub fn use_device_prefs() -> DevicePrefsHandle {
    use_context::<DevicePrefsHandle>()
}

/// `localStorage`, best effort: private modes and full quotas only lose the preference.
#[cfg(feature = "web")]
mod storage {
    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn read(key: &str) -> Option<String> {
        local_storage()?.get_item(key).ok().flatten()
    }

    pub fn write(key: &str, value: &str) {
        if let Some(storage) = local_storage() {
            // A refused write keeps the preference for this visit only.
            let _ = storage.set_item(key, value);
        }
    }

    pub fn remove(key: &str) {
        if let Some(storage) = local_storage() {
            let _ = storage.remove_item(key);
        }
    }
}

/// No storage outside the browser.
#[cfg(not(feature = "web"))]
mod storage {
    pub const fn read(_key: &str) -> Option<String> {
        None
    }

    pub const fn write(_key: &str, _value: &str) {}

    pub const fn remove(_key: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    #[test]
    fn defaults_are_two_and_a_half_kg_or_five_lb_and_vibration_on() {
        let prefs = DevicePrefs::default();
        assert_eq!(prefs.weight_step(Unit::Kg), kg(2.5));
        assert_eq!(prefs.weight_step(Unit::Lb), Weight::from_lb(5.0).unwrap());
        assert!(prefs.vibration);
    }

    #[test]
    fn each_unit_keeps_its_own_step() {
        let prefs = DevicePrefs::default().with_weight_step(Unit::Kg, kg(1.25));
        assert_eq!(prefs.weight_step(Unit::Kg), kg(1.25));
        assert_eq!(prefs.weight_step(Unit::Lb), default_step(Unit::Lb));
        // A zero step would never move: it falls back to the default.
        let zero = DevicePrefs::default().with_weight_step(Unit::Lb, Weight::ZERO);
        assert_eq!(zero.weight_step(Unit::Lb), default_step(Unit::Lb));
    }

    #[test]
    fn stored_preferences_round_trip() {
        let prefs = DevicePrefs {
            kg_step: Some(kg(1.0)),
            lb_step: None,
            vibration: false,
        };
        assert_eq!(DevicePrefs::parse(Some(&prefs.to_stored())), prefs);
    }

    #[test]
    fn missing_or_unreadable_storage_gives_the_defaults() {
        assert_eq!(DevicePrefs::parse(None), DevicePrefs::default());
        assert_eq!(DevicePrefs::parse(Some("not json")), DevicePrefs::default());
        assert_eq!(DevicePrefs::parse(Some("{}")), DevicePrefs::default());
        // A step that is not a valid weight drops the whole entry rather than half of it.
        assert_eq!(
            DevicePrefs::parse(Some(r#"{"kg_step": -1, "vibration": false}"#)),
            DevicePrefs::default()
        );
        assert!(!DevicePrefs::parse(Some(r#"{"vibration": false}"#)).vibration);
    }

    #[test]
    fn each_user_has_their_own_key() {
        let a = UserId::from_uuid(uuid::Uuid::from_u128(1));
        let b = UserId::from_uuid(uuid::Uuid::from_u128(2));
        assert_ne!(storage_key(a), storage_key(b));
        assert!(storage_key(a).starts_with("iron-oxide.device-prefs.v1."));
        assert!(storage_key(a).ends_with(&a.to_string()));
    }

    #[test]
    fn every_step_choice_is_a_valid_weight() {
        for unit in Unit::ALL {
            for &value in step_choices(unit) {
                assert!(Weight::new(value, unit).is_ok(), "{value} {unit}");
            }
            let default = default_step(unit);
            assert!(
                step_choices(unit)
                    .iter()
                    .any(|&value| Weight::new(value, unit).unwrap() == default)
            );
        }
    }
}
