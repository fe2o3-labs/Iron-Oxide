//! Browser-side sign-in calls (web build only): WebAuthn through `navigator.credentials`, and the
//! Google sign-in popup with its callback listener.
//!
//! Only the JavaScript glue lives here; the conversions and checks are in the target-independent
//! [`webauthn_json`](super::webauthn_json), [`google_popup`](super::google_popup) and
//! [`browser_error`](super::browser_error) modules. Every JavaScript access is fallible and
//! mapped to a [`BrowserError`].

use js_sys::{Array, ArrayBuffer, Function, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    BroadcastChannel, CredentialCreationOptions, CredentialRequestOptions, DomException,
    MessageEvent, Window,
};
use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

pub use super::browser_error::BrowserError;
use super::browser_error::Ceremony;
use super::google_popup::{accept_callback_message, is_navigable_url};
use super::types::{GOOGLE_CALLBACK_CHANNEL, GoogleCallbackMessage};
use super::webauthn_json::{
    BrowserOptions, PathStep, RawAssertion, RawRegistration, assertion_credential,
    creation_options, registration_credential, request_options,
};

/// Name of the popup window, so a second tap reuses it instead of opening another.
const POPUP_NAME: &str = "iron-oxide-google";
/// Size hint for desktop browsers; mobile browsers open a tab and ignore it.
const POPUP_FEATURES: &str = "popup,width=500,height=680";

fn window() -> Result<Window, BrowserError> {
    web_sys::window().ok_or(BrowserError::Unsupported)
}

fn malformed(what: &str) -> BrowserError {
    BrowserError::Malformed(what.to_owned())
}

fn get(target: &JsValue, key: &str) -> Result<JsValue, BrowserError> {
    Reflect::get(target, &JsValue::from_str(key)).map_err(|_| malformed(key))
}

/// Builds the options object: the JSON, with a `Uint8Array` at every binary field.
fn options_object(options: &BrowserOptions) -> Result<JsValue, BrowserError> {
    let root =
        js_sys::JSON::parse(&options.json.to_string()).map_err(|_| malformed("options JSON"))?;
    for field in &options.binary_fields {
        let Some((last, parents)) = field.path.split_last() else {
            return Err(malformed("empty binary field path"));
        };
        let mut target = root.clone();
        for step in parents {
            target = match step {
                PathStep::Key(key) => get(&target, key)?,
                PathStep::Index(index) => {
                    let index = u32::try_from(*index).map_err(|_| malformed("index"))?;
                    Reflect::get_u32(&target, index).map_err(|_| malformed("index"))?
                }
            };
            if !target.is_object() {
                return Err(malformed("binary field parent"));
            }
        }
        let bytes = Uint8Array::from(field.bytes.as_slice());
        let set = match last {
            PathStep::Key(key) => Reflect::set(&target, &JsValue::from_str(key), &bytes),
            PathStep::Index(index) => {
                let index = u32::try_from(*index).map_err(|_| malformed("index"))?;
                Reflect::set_u32(&target, index, &bytes)
            }
        };
        if !set.map_err(|_| malformed("binary field"))? {
            return Err(malformed("binary field"));
        }
    }
    Ok(root)
}

/// Whether this browser has WebAuthn at all.
fn check_supported(window: &Window) -> Result<(), BrowserError> {
    let has_api = Reflect::has(window, &JsValue::from_str("PublicKeyCredential")).unwrap_or(false);
    let has_container = get(&window.navigator(), "credentials")
        .map(|credentials| credentials.is_object())
        .unwrap_or(false);
    if has_api && has_container {
        Ok(())
    } else {
        Err(BrowserError::Unsupported)
    }
}

/// Maps a rejected `create()`/`get()` promise.
fn rejection(ceremony: Ceremony, error: &JsValue) -> BrowserError {
    let name = if let Some(exception) = error.dyn_ref::<DomException>() {
        exception.name()
    } else if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        String::from(error.name())
    } else {
        String::new()
    };
    BrowserError::from_exception_name(ceremony, &name)
}

/// Awaits a WebAuthn promise and returns the credential object (never `null`).
async fn credential(ceremony: Ceremony, promise: js_sys::Promise) -> Result<JsValue, BrowserError> {
    let credential = JsFuture::from(promise)
        .await
        .map_err(|error| rejection(ceremony, &error))?;
    if credential.is_object() {
        Ok(credential)
    } else {
        // A `null` result means no credential was made or chosen.
        Err(BrowserError::Cancelled)
    }
}

/// The bytes of a required `ArrayBuffer` member.
fn buffer(target: &JsValue, key: &str) -> Result<Vec<u8>, BrowserError> {
    optional_buffer(target, key)?.ok_or_else(|| malformed(key))
}

/// The bytes of an `ArrayBuffer` member; `None` if it is `null` or absent.
fn optional_buffer(target: &JsValue, key: &str) -> Result<Option<Vec<u8>>, BrowserError> {
    let value = get(target, key)?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    let buffer = value
        .dyn_ref::<ArrayBuffer>()
        .ok_or_else(|| malformed(key))?;
    Ok(Some(Uint8Array::new(buffer).to_vec()))
}

/// Calls `target[method]()` if it is a function; `None` if the browser lacks it or it throws.
fn call_optional(target: &JsValue, method: &str) -> Option<JsValue> {
    let function = get(target, method).ok()?.dyn_into::<Function>().ok()?;
    function.call0(target).ok()
}

/// `response.getTransports()`, as strings.
fn transports(response: &JsValue) -> Option<Vec<String>> {
    let transports = call_optional(response, "getTransports")?;
    let transports = transports.dyn_ref::<Array>()?;
    Some(transports.iter().filter_map(|t| t.as_string()).collect())
}

/// `getClientExtensionResults().credProps.rk`.
fn cred_props_rk(credential: &JsValue) -> Option<bool> {
    let results = call_optional(credential, "getClientExtensionResults")?;
    let cred_props = get(&results, "credProps").ok()?;
    if !cred_props.is_object() {
        return None;
    }
    get(&cred_props, "rk").ok()?.as_bool()
}

/// Creates a passkey with `navigator.credentials.create()`.
pub async fn create_passkey(
    options: CreationChallengeResponse,
) -> Result<RegisterPublicKeyCredential, BrowserError> {
    let window = window()?;
    check_supported(&window)?;
    let options = options_object(&creation_options(&options)?)?;
    let promise = window
        .navigator()
        .credentials()
        .create_with_options(options.unchecked_ref::<CredentialCreationOptions>())
        .map_err(|error| rejection(Ceremony::Create, &error))?;
    let credential = credential(Ceremony::Create, promise).await?;
    let response = get(&credential, "response")?;
    let raw = RawRegistration {
        raw_id: buffer(&credential, "rawId")?,
        client_data_json: buffer(&response, "clientDataJSON")?,
        attestation_object: buffer(&response, "attestationObject")?,
        transports: transports(&response),
        cred_props_rk: cred_props_rk(&credential),
    };
    Ok(registration_credential(raw)?)
}

/// Signs in with a passkey through `navigator.credentials.get()`.
pub async fn get_passkey(
    options: RequestChallengeResponse,
) -> Result<PublicKeyCredential, BrowserError> {
    let window = window()?;
    check_supported(&window)?;
    let options = options_object(&request_options(&options)?)?;
    let promise = window
        .navigator()
        .credentials()
        .get_with_options(options.unchecked_ref::<CredentialRequestOptions>())
        .map_err(|error| rejection(Ceremony::Get, &error))?;
    let credential = credential(Ceremony::Get, promise).await?;
    let response = get(&credential, "response")?;
    let raw = RawAssertion {
        raw_id: buffer(&credential, "rawId")?,
        client_data_json: buffer(&response, "clientDataJSON")?,
        authenticator_data: buffer(&response, "authenticatorData")?,
        signature: buffer(&response, "signature")?,
        user_handle: optional_buffer(&response, "userHandle")?,
    };
    Ok(assertion_credential(raw)?)
}

/// Waits `ms` milliseconds (`setTimeout`). Returns at once if there is no window.
pub async fn sleep(ms: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = JsFuture::from(promise).await;
}

/// Asks the user to confirm with `window.confirm`. `false` if it cannot be shown.
#[must_use]
pub fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(false)
}

/// The Google sign-in popup, opened before the authorization URL is known.
///
/// Must be created synchronously in the tap's event handler, before any `.await`: iOS Safari
/// only allows popups opened during the user gesture.
pub struct GooglePopup {
    /// `None` when the browser blocked the popup: the flow then uses this window instead.
    window: Option<Window>,
}

/// Where the Google page was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleNavigation {
    /// In the popup: wait for its callback message.
    Popup,
    /// In this window (popup blocked): the page is going away.
    Redirect,
}

impl GooglePopup {
    /// Opens an empty popup. Never fails: a blocked popup falls back to a full redirect later.
    #[must_use]
    pub fn open() -> Self {
        let window = web_sys::window().and_then(|window| {
            window
                .open_with_url_and_target_and_features("", POPUP_NAME, POPUP_FEATURES)
                .ok()
                .flatten()
        });
        Self { window }
    }

    /// No popup: the flow runs in this window (a full-page redirect).
    #[must_use]
    pub fn this_window() -> Self {
        Self { window: None }
    }

    /// Whether the browser let us open the popup (otherwise the flow uses this window).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.window.is_some()
    }

    /// Sends the popup to `url`, or this window if there is no popup.
    pub fn navigate(&self, url: &str) -> Result<GoogleNavigation, BrowserError> {
        if !is_navigable_url(url) {
            self.close();
            return Err(malformed("Google URL"));
        }
        if let Some(popup) = &self.window
            && popup.location().set_href(url).is_ok()
        {
            return Ok(GoogleNavigation::Popup);
        }
        self.close();
        window()?
            .location()
            .assign(url)
            .map_err(|_| malformed("redirect"))?;
        Ok(GoogleNavigation::Redirect)
    }

    /// Closes the popup, if it is still ours to close.
    pub fn close(&self) {
        if let Some(popup) = &self.window {
            // Fails silently once the popup is cross-origin or already closed.
            let _ = popup.close();
        }
    }
}

/// Listens for the Google callback page's message, on `window` (`postMessage` from the popup)
/// and on the `BroadcastChannel` (when the popup lost its opener). Removes both listeners when
/// dropped.
pub struct GoogleCallbackListener {
    window: Option<Window>,
    on_window_message: Closure<dyn FnMut(MessageEvent)>,
    channel: Option<BroadcastChannel>,
    /// Kept alive for as long as the channel may call it.
    _on_channel_message: Closure<dyn FnMut(MessageEvent)>,
}

impl GoogleCallbackListener {
    /// Calls `on_message` with each valid callback message: from our own origin, a string
    /// holding a JSON `GoogleCallbackMessage`. Everything else is ignored.
    ///
    /// The same message may arrive on both paths; the caller de-duplicates.
    pub fn install(on_message: impl Fn(GoogleCallbackMessage) + 'static) -> Self {
        let on_message = std::rc::Rc::new(on_message);
        let window = web_sys::window();
        let own_origin = window
            .as_ref()
            .and_then(|window| window.location().origin().ok())
            .unwrap_or_default();

        let handler = |on_message: std::rc::Rc<dyn Fn(GoogleCallbackMessage)>,
                       own_origin: String| {
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let data = event.data().as_string();
                if let Some(message) =
                    accept_callback_message(&event.origin(), &own_origin, data.as_deref())
                {
                    on_message(message);
                }
            })
        };
        let on_window_message = handler(on_message.clone(), own_origin.clone());
        let on_channel_message = handler(on_message, own_origin);

        if let Some(window) = &window {
            let _ = window.add_event_listener_with_callback(
                "message",
                on_window_message.as_ref().unchecked_ref(),
            );
        }
        // Missing on old browsers; the `postMessage` path still works there.
        let channel = BroadcastChannel::new(GOOGLE_CALLBACK_CHANNEL).ok();
        if let Some(channel) = &channel {
            channel.set_onmessage(Some(on_channel_message.as_ref().unchecked_ref()));
        }

        Self {
            window,
            on_window_message,
            channel,
            _on_channel_message: on_channel_message,
        }
    }
}

impl Drop for GoogleCallbackListener {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            let _ = window.remove_event_listener_with_callback(
                "message",
                self.on_window_message.as_ref().unchecked_ref(),
            );
        }
        if let Some(channel) = &self.channel {
            channel.set_onmessage(None);
            channel.close();
        }
        // The closures are no longer referenced by the page and drop with `self`.
    }
}
