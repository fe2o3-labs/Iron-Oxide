//! The banner at the top of every page, fed by the shared [`Errors`](crate::ui::errors::Errors).

use dioxus::prelude::*;

use super::IconButton;
use super::icons::CloseIcon;
use crate::ui::errors::{BannerKind, use_errors};

/// Shows the current banner, if any, with a button to dismiss it. Errors are announced at once
/// (`role="alert"`), notes politely (`role="status"`).
#[component]
pub fn BannerHost() -> Element {
    let errors = use_errors();
    let Some(banner) = errors.banner() else {
        return rsx! {};
    };
    let (class, role) = match banner.kind {
        BannerKind::Error => ("io-banner", "alert"),
        BannerKind::Info => ("io-banner io-banner-info", "status"),
        BannerKind::Warning => ("io-banner io-banner-warning", "status"),
    };
    rsx! {
        div { class: "io-banners",
            div { key: "{banner.id}", class, role,
                p { "{banner.message}" }
                IconButton { label: "Dismiss", onclick: move |_| errors.dismiss(), CloseIcon {} }
            }
        }
    }
}
