//! The user's time zone, for the dates of the history.

/// Minutes east of UTC in the browser's time zone at `at_ms` (ms since the epoch). Taken at that
/// instant, not now, so a session near midnight lands on the right day across a DST change.
///
/// UTC outside the browser: the history is only rendered client side, after sign-in.
#[must_use]
pub fn offset_minutes(at_ms: i64) -> i32 {
    #[cfg(feature = "web")]
    {
        // Exact: every date we show is far below 2⁵³ ms.
        #[allow(clippy::cast_precision_loss)]
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_ms as f64));
        // `getTimezoneOffset` is minutes *behind* UTC, a whole number well within ±24 h.
        #[allow(clippy::cast_possible_truncation)]
        let behind = date.get_timezone_offset() as i32;
        -behind
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = at_ms;
        0
    }
}
