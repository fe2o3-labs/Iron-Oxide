//! Buttons: at least 56 px high, the big action of a screen 76 px; icon buttons 44 px.

use dioxus::prelude::*;

/// How a [`Button`] looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// The main action: accent in dark, ink in light.
    #[default]
    Primary,
    /// A second action, on a raised fill.
    Secondary,
    /// A quiet action: outline and accent text.
    Ghost,
    /// A destructive action: outline and danger text.
    Danger,
}

impl ButtonVariant {
    const fn class(self) -> &'static str {
        match self {
            Self::Primary => "io-button-primary",
            Self::Secondary => "io-button-secondary",
            Self::Ghost => "io-button-ghost",
            Self::Danger => "io-button-danger",
        }
    }
}

/// A text button, at least 56 px high. `xl` makes it the 76 px action of a screen (Done), `block`
/// makes it full width. While `busy`, it is disabled and announced as busy.
#[component]
pub fn Button(
    #[props(default)] variant: ButtonVariant,
    #[props(default)] xl: bool,
    #[props(default)] block: bool,
    #[props(default)] disabled: bool,
    #[props(default)] busy: bool,
    #[props(into)] id: Option<String>,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let mut class = format!("io-button {}", variant.class());
    if xl {
        class.push_str(" io-button-xl");
    }
    if block {
        class.push_str(" io-button-block");
    }
    rsx! {
        button {
            r#type: "button",
            id,
            class,
            disabled: disabled || busy,
            "aria-busy": busy,
            onclick: move |event| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}

/// A 44 px square button holding an icon, for headers. `label` is its accessible name.
#[component]
pub fn IconButton(
    #[props(into)] label: String,
    #[props(default)] disabled: bool,
    onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "io-icon-button",
            "aria-label": label,
            disabled,
            onclick: move |event| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}
