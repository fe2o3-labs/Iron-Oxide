//! A bottom sheet: a modal panel over the page for a short side task (the plate calculator).
//!
//! It is a native `<dialog>` opened with `showModal()`, so the browser traps the focus in it,
//! makes the page behind it inert and handles Escape. Closing it, by any means, calls `on_close`
//! and the owner stops rendering it:
//! - the close button, Escape, a tap on the backdrop;
//! - the back gesture or button: opening it pushes a history entry, and going back closes it
//!   instead of leaving the page. When it is closed another way, that entry is popped.
//!
//! When it goes away, the focus returns to the element that opened it.

use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::prelude::*;

use super::IconButton;
use super::icons::CloseIcon;

// The two scripts below are fixed text run through `document::eval`; the only value spliced in is
// the sheet's own generated id (`io-sheet-<n>`), never user input.

/// Ids for the sheets' `<dialog>` elements, unique on the page.
static NEXT_SHEET: AtomicU64 = AtomicU64::new(0);

/// Opens the dialog `id` as a modal, remembers the focused element, pushes a history entry, and
/// sends `"close"` when the user closes it (Escape, back).
fn open_script(id: &str) -> String {
    format!(
        r#"
const dialog = document.getElementById("{id}");
const opener = document.activeElement;
const sheets = (window.__ioSheets = window.__ioSheets || {{}});
const onPop = () => {{ cleanup(); dioxus.send("close"); }};
const onCancel = (event) => {{ event.preventDefault(); dioxus.send("close"); }};
const onClose = () => {{ dioxus.send("close"); }};
function cleanup() {{
  window.removeEventListener("popstate", onPop);
  dialog.removeEventListener("cancel", onCancel);
  dialog.removeEventListener("close", onClose);
}}
sheets["{id}"] = {{ cleanup, opener }};
if (!dialog.open) dialog.showModal();
history.pushState({{ ioSheet: "{id}" }}, "");
window.addEventListener("popstate", onPop);
dialog.addEventListener("cancel", onCancel);
dialog.addEventListener("close", onClose);
"#
    )
}

/// Undoes [`open_script`] once the sheet is gone: stops listening, pops its history entry if it
/// is still the current one (closed another way than back), and gives the focus back.
fn close_script(id: &str) -> String {
    format!(
        r#"
const sheets = window.__ioSheets || {{}};
const sheet = sheets["{id}"];
if (sheet) {{
  sheet.cleanup();
  delete sheets["{id}"];
  if (history.state && history.state.ioSheet === "{id}") history.back();
  if (sheet.opener && sheet.opener.isConnected && sheet.opener.focus) sheet.opener.focus();
}}
"#
    )
}

/// A modal bottom sheet titled `title`. The close button, Escape, the back gesture and a tap on
/// the backdrop call `on_close`. Render it only while it is open.
#[component]
pub fn Sheet(
    #[props(into)] title: String,
    on_close: EventHandler<()>,
    children: Element,
) -> Element {
    let id = use_hook(|| format!("io-sheet-{}", NEXT_SHEET.fetch_add(1, Ordering::Relaxed)));
    let title_id = format!("{id}-title");

    {
        let id = id.clone();
        use_effect(move || {
            let mut opened = document::eval(&open_script(&id));
            spawn(async move {
                // One message per close request; the owner then stops rendering the sheet.
                while opened.recv::<String>().await.is_ok() {
                    on_close.call(());
                }
            });
        });
    }
    {
        let id = id.clone();
        use_drop(move || {
            document::eval(&close_script(&id));
        });
    }

    rsx! {
        dialog {
            id: "{id}",
            class: "io-sheet",
            aria_labelledby: "{title_id}",
            // A tap on the backdrop lands on the dialog itself; taps on the panel stop there.
            onclick: move |_| on_close.call(()),
            div { class: "io-sheet-panel", onclick: move |event| event.stop_propagation(),
                div { class: "io-sheet-header",
                    h2 { id: "{title_id}", class: "io-label", "{title}" }
                    IconButton { label: "Close", onclick: move |_| on_close.call(()), CloseIcon {} }
                }
                {children}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_target_their_own_sheet() {
        let open = open_script("io-sheet-7");
        assert!(open.contains(r#"getElementById("io-sheet-7")"#));
        assert!(open.contains("showModal()"));
        assert!(open.contains(r#"history.pushState({ ioSheet: "io-sheet-7" }"#));
        let close = close_script("io-sheet-7");
        assert!(close.contains(r#"history.state.ioSheet === "io-sheet-7""#));
        assert!(close.contains("history.back()"));
        assert!(close.contains("opener.focus()"));
    }
}
