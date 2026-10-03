//! What the outbox needs from the browser: the clock, randomness, `localStorage`, the `online`
//! and `storage` events, and a cross-tab lock (Web Locks API).
//!
//! Outside the web build (the server, host tests), the same API with stand-ins: the system
//! clock, a memory store and no events. Nothing there runs in practice, since the outbox only
//! works from client-side effects.

use std::time::Duration;

use iron_oxide_domain::time::Timestamp;

use super::storage::MemoryStorage;

thread_local! {
    /// Where the outbox lives when `localStorage` is unavailable (or outside the browser).
    static MEMORY: MemoryStorage = MemoryStorage::volatile();
}

/// Something the browser tells the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEvent {
    /// `online`: the connection is back.
    Online,
    /// `storage`: another tab changed this key (`None`: storage was cleared).
    StorageChanged(Option<String>),
}

/// Waits `delay` (at most about 24 days; longer delays are cut there).
pub async fn sleep(delay: Duration) {
    let ms = i32::try_from(delay.as_millis()).unwrap_or(i32::MAX);
    crate::auth::browser::sleep(ms).await;
}

#[cfg(feature = "web")]
mod imp {
    use iron_oxide_domain::time::Timestamp;
    use js_sys::{Array, Function, Object, Promise, Reflect};
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Event, Window};

    use super::{BrowserEvent, MEMORY};
    use crate::offline::storage::{Storage, StorageError};

    /// The browser's clock (`Date.now()`).
    pub fn now() -> Timestamp {
        // Milliseconds since the epoch fit exactly in an f64 (up to 2^53).
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Date.now() is a whole number of milliseconds"
        )]
        Timestamp::from_epoch_millis(js_sys::Date::now() as i64)
    }

    /// A uniform draw in `[0, 1)` (`Math.random()`).
    pub fn random() -> f64 {
        js_sys::Math::random()
    }

    /// `window.localStorage`, when the browser allows it.
    struct LocalStorage(web_sys::Storage);

    fn error(value: &JsValue) -> StorageError {
        StorageError(
            Reflect::get(value, &JsValue::from_str("name"))
                .ok()
                .and_then(|name| name.as_string())
                .unwrap_or_else(|| "storage error".to_owned()),
        )
    }

    impl Storage for LocalStorage {
        fn get(&self, key: &str) -> Result<Option<String>, StorageError> {
            self.0.get_item(key).map_err(|e| error(&e))
        }

        fn set(&self, key: &str, value: &str) -> Result<(), StorageError> {
            self.0.set_item(key, value).map_err(|e| error(&e))
        }

        fn remove(&self, key: &str) -> Result<(), StorageError> {
            self.0.remove_item(key).map_err(|e| error(&e))
        }
    }

    /// Runs `f` with `localStorage`, or with the memory store when it is blocked.
    pub fn with_storage<R>(f: impl FnOnce(&dyn Storage) -> R) -> R {
        match web_sys::window().and_then(|window| window.local_storage().ok().flatten()) {
            Some(storage) => f(&LocalStorage(storage)),
            None => MEMORY.with(|memory| f(memory)),
        }
    }

    /// Forwards the window's `online` and `storage` events while alive.
    pub struct Listeners {
        window: Option<Window>,
        on_online: Closure<dyn FnMut(Event)>,
        on_storage: Closure<dyn FnMut(Event)>,
    }

    impl Listeners {
        pub fn install(forward: impl Fn(BrowserEvent) + 'static) -> Self {
            let forward = std::rc::Rc::new(forward);
            let online = forward.clone();
            let on_online = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                online(BrowserEvent::Online);
            });
            let on_storage = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                let key = Reflect::get(&event, &JsValue::from_str("key"))
                    .ok()
                    .and_then(|key| key.as_string());
                forward(BrowserEvent::StorageChanged(key));
            });
            let window = web_sys::window();
            if let Some(window) = &window {
                let _ = window
                    .add_event_listener_with_callback("online", on_online.as_ref().unchecked_ref());
                let _ = window.add_event_listener_with_callback(
                    "storage",
                    on_storage.as_ref().unchecked_ref(),
                );
            }
            Self {
                window,
                on_online,
                on_storage,
            }
        }
    }

    impl Drop for Listeners {
        fn drop(&mut self) {
            if let Some(window) = &self.window {
                let _ = window.remove_event_listener_with_callback(
                    "online",
                    self.on_online.as_ref().unchecked_ref(),
                );
                let _ = window.remove_event_listener_with_callback(
                    "storage",
                    self.on_storage.as_ref().unchecked_ref(),
                );
            }
        }
    }

    /// Held while this tab drains the outbox; releases the lock when dropped.
    pub struct LockGuard(Option<Function>);

    impl Drop for LockGuard {
        fn drop(&mut self) {
            if let Some(release) = &self.0 {
                let _ = release.call0(&JsValue::NULL);
            }
        }
    }

    /// A promise and the function that resolves it.
    fn deferred() -> (Promise, Option<Function>) {
        let mut resolver = None;
        let promise = Promise::new(&mut |resolve, _reject| resolver = Some(resolve));
        (promise, resolver)
    }

    /// Takes the cross-tab lock `name` if no other tab holds it (`navigator.locks.request` with
    /// `ifAvailable`). `None`: another tab holds it. Without the Web Locks API (old browsers,
    /// insecure origins), always succeeds: tabs may then drain at the same time, which only
    /// sends a write twice (the server answers a replay unchanged).
    pub async fn try_lock(name: &str) -> Option<LockGuard> {
        let unlocked = Some(LockGuard(None));
        let Some(navigator) = web_sys::window().map(|window| window.navigator()) else {
            return unlocked;
        };
        let locks = Reflect::get(&navigator, &JsValue::from_str("locks")).unwrap_or_default();
        let Some(request) = Reflect::get(&locks, &JsValue::from_str("request"))
            .ok()
            .and_then(|request| request.dyn_into::<Function>().ok())
        else {
            return unlocked;
        };

        // `granted` resolves with whether the lock was given; `held` keeps it until released.
        let (granted, grant) = deferred();
        let (held, release) = deferred();
        let (Some(grant), Some(release)) = (grant, release) else {
            return unlocked;
        };
        let callback = Closure::once_into_js(move |lock: JsValue| -> Promise {
            let _ = grant.call1(&JsValue::NULL, &JsValue::from_bool(!lock.is_null()));
            if lock.is_null() {
                Promise::resolve(&JsValue::UNDEFINED)
            } else {
                held
            }
        });
        let options = Object::new();
        let _ = Reflect::set(&options, &JsValue::from_str("ifAvailable"), &JsValue::TRUE);
        let Ok(pending) = request.call3(&locks, &JsValue::from_str(name), &options, &callback)
        else {
            return unlocked;
        };
        // The request's own promise only settles once the lock is released, or rejects at once
        // (a security error): whichever comes first tells.
        let pending = Promise::resolve(&pending);
        match JsFuture::from(Promise::race(&Array::of2(&granted, &pending))).await {
            Ok(value) if value.as_bool() == Some(true) => Some(LockGuard(Some(release))),
            Ok(value) if value.as_bool() == Some(false) => {
                let _ = release.call0(&JsValue::NULL);
                None
            }
            _ => {
                let _ = release.call0(&JsValue::NULL);
                unlocked
            }
        }
    }
}

#[cfg(not(feature = "web"))]
mod imp {
    use iron_oxide_domain::time::Timestamp;

    use super::{BrowserEvent, MEMORY};
    use crate::offline::storage::Storage;

    /// The system clock.
    pub fn now() -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        Timestamp::from_epoch_millis(millis)
    }

    /// The middle of the jitter range: no randomness source outside the browser.
    pub const fn random() -> f64 {
        0.5
    }

    /// The memory store.
    pub fn with_storage<R>(f: impl FnOnce(&dyn Storage) -> R) -> R {
        MEMORY.with(|memory| f(memory))
    }

    /// No browser events outside the browser.
    pub struct Listeners;

    impl Listeners {
        pub fn install(_forward: impl Fn(BrowserEvent) + 'static) -> Self {
            Self
        }
    }

    /// No other tabs outside the browser.
    pub struct LockGuard;

    pub async fn try_lock(_name: &str) -> Option<LockGuard> {
        Some(LockGuard)
    }
}

pub use imp::{Listeners, now, random, try_lock, with_storage};

/// The milliseconds from `now` to `at`, zero if it has passed.
#[must_use]
pub fn until(at: Timestamp, now: Timestamp) -> Duration {
    at.saturating_duration_since(now)
}
