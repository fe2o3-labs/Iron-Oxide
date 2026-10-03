//! The browser calls of the session screens: the clock and a little `localStorage`.
//!
//! Outside the browser (the server render, host tests) the clock is the system's and storage is
//! absent; nothing here is called during a render, only from event handlers and client effects.

use iron_oxide_domain::time::Timestamp;

/// The client's clock (`Date.now()` in the browser).
#[must_use]
pub fn now() -> Timestamp {
    #[cfg(feature = "web")]
    {
        // Milliseconds since the epoch fit in an i64 for the next 290 million years.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Date.now() is a whole number of ms"
        )]
        Timestamp::from_epoch_millis(js_sys::Date::now() as i64)
    }
    #[cfg(not(feature = "web"))]
    {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        Timestamp::from_epoch_millis(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}

/// Reads a `localStorage` entry. `None` when absent or when storage is unavailable (private
/// mode, blocked site data).
#[must_use]
pub fn load(key: &str) -> Option<String> {
    #[cfg(feature = "web")]
    {
        storage()?.get_item(key).ok().flatten()
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = key;
        None
    }
}

/// Writes a `localStorage` entry, best effort: the entries kept here are conveniences that the
/// app can do without.
pub fn store(key: &str, value: &str) {
    #[cfg(feature = "web")]
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
    #[cfg(not(feature = "web"))]
    let _ = (key, value);
}

/// Removes a `localStorage` entry, best effort.
pub fn remove(key: &str) {
    #[cfg(feature = "web")]
    if let Some(storage) = storage() {
        let _ = storage.remove_item(key);
    }
    #[cfg(not(feature = "web"))]
    let _ = key;
}

#[cfg(feature = "web")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
