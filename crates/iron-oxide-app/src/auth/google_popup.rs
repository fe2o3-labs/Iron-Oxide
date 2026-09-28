//! Target-independent checks for the Google sign-in popup: which callback messages to accept,
//! and which URLs the popup may be sent to.

use super::types::GoogleCallbackMessage;

/// Longest callback message accepted, in bytes. A real one is a few hundred bytes.
const MAX_MESSAGE_LEN: usize = 8 * 1024;

/// The callback message carried by a `message` event, or `None` if the event must be ignored.
///
/// Accepts only events from our own origin whose data is a string holding a JSON
/// [`GoogleCallbackMessage`] (the callback page posts `JSON.stringify(message)`).
#[must_use]
pub fn accept_callback_message(
    event_origin: &str,
    own_origin: &str,
    data: Option<&str>,
) -> Option<GoogleCallbackMessage> {
    // `null` is the serialization of an opaque origin; never ours.
    if own_origin.is_empty() || own_origin == "null" || event_origin != own_origin {
        return None;
    }
    let data = data?;
    if data.len() > MAX_MESSAGE_LEN {
        return None;
    }
    // Only the object form: serde would also accept a tagged enum written as an array.
    match serde_json::from_str::<serde_json::Value>(data).ok()? {
        object @ serde_json::Value::Object(_) => serde_json::from_value(object).ok(),
        _ => None,
    }
}

/// Whether the popup (or this window) may be navigated to `url`: only plain `https://` or
/// `http://` URLs, so a bad server answer can never become a `javascript:` or `data:` URL
/// running in our origin.
#[must_use]
pub fn is_navigable_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() > "https://".len()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://iron-oxide.example";

    #[test]
    fn accepts_each_message_kind_from_our_origin() {
        assert_eq!(
            accept_callback_message(ORIGIN, ORIGIN, Some(r#"{"type":"done"}"#)),
            Some(GoogleCallbackMessage::Done)
        );
        assert_eq!(
            accept_callback_message(ORIGIN, ORIGIN, Some(r#"{"type":"relayed"}"#)),
            Some(GoogleCallbackMessage::Relayed)
        );
        assert_eq!(
            accept_callback_message(ORIGIN, ORIGIN, Some(r#"{"type":"error","message":"no"}"#)),
            Some(GoogleCallbackMessage::Error {
                message: "no".to_owned()
            })
        );
    }

    #[test]
    fn ignores_other_origins() {
        for origin in [
            "https://evil.example",
            "https://iron-oxide.example.evil.example",
            "http://iron-oxide.example",
            "https://iron-oxide.example:444",
            "null",
            "",
        ] {
            assert_eq!(
                accept_callback_message(origin, ORIGIN, Some(r#"{"type":"done"}"#)),
                None,
                "{origin}"
            );
        }
    }

    #[test]
    fn ignores_everything_when_our_origin_is_opaque() {
        assert_eq!(
            accept_callback_message("null", "null", Some(r#"{"type":"done"}"#)),
            None
        );
        assert_eq!(
            accept_callback_message("", "", Some(r#"{"type":"done"}"#)),
            None
        );
    }

    #[test]
    fn ignores_non_string_or_unparsable_data() {
        for data in [
            None,
            Some(""),
            Some("done"),
            Some(r#"{"type":"unknown"}"#),
            Some(r#"{"type":"error"}"#),
            Some(r#"["done"]"#),
        ] {
            assert_eq!(
                accept_callback_message(ORIGIN, ORIGIN, data),
                None,
                "{data:?}"
            );
        }
    }

    #[test]
    fn ignores_oversized_messages() {
        let message = "m".repeat(MAX_MESSAGE_LEN);
        let data = format!(r#"{{"type":"error","message":"{message}"}}"#);
        assert_eq!(accept_callback_message(ORIGIN, ORIGIN, Some(&data)), None);
    }

    #[test]
    fn navigable_urls_are_plain_http_or_https() {
        assert!(is_navigable_url(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=x&state=y"
        ));
        assert!(is_navigable_url("http://localhost:9000/authorize?x=1"));
        for url in [
            "",
            "https://",
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            " https://accounts.google.com",
            "java\tscript:alert(1)",
            "data:text/html,hi",
            "//evil.example",
            "/relative",
            "HTTPS://accounts.google.com",
            "https://accounts.google.com/\nx",
            "https://accounts.google.com/ x",
        ] {
            assert!(!is_navigable_url(url), "{url:?}");
        }
    }
}
