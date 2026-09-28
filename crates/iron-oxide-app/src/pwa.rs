//! Progressive Web App wiring: web manifest, icons, iOS meta tags and service worker registration.
//!
//! The manifest, the service worker and the icons are static files in `public/`, which `dx` copies
//! unchanged to the root of the served site (`/manifest.webmanifest`, `/sw.js`, `/icons/…`). The
//! service worker must stay at `/sw.js` so that its scope covers the whole app.

use dioxus::prelude::*;

/// Colour of the browser UI around the app. Keep in sync with `--io-bg` in `assets/tokens.css`
/// and with `theme_color` / `background_color` in `public/manifest.webmanifest`.
pub const THEME_COLOR: &str = "#141619";

/// Registers `/sw.js` once the page has loaded. It runs as a plain inline script, so it does not
/// wait for the wasm bundle.
const REGISTER_SERVICE_WORKER: &str = r#"
if ("serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js", { scope: "/" }).catch((error) => {
      console.error("Service worker registration failed", error);
    });
  });
}
"#;

/// Whether this build registers the service worker. Debug builds (`dx serve`) serve the wasm
/// from an unhashed `/wasm/` folder and rebuild constantly, so caching would only get in the way.
const REGISTER_IN_THIS_BUILD: bool = !cfg!(debug_assertions);

/// Head elements that make the app installable.
#[component]
pub fn PwaHead() -> Element {
    rsx! {
        document::Link { rel: "manifest", href: "/manifest.webmanifest" }
        document::Meta { name: "theme-color", content: THEME_COLOR }
        document::Link { rel: "icon", href: "/favicon.ico", sizes: "48x48" }
        document::Link {
            rel: "icon",
            href: "/icons/favicon.svg",
            r#type: "image/svg+xml",
        }
        document::Link { rel: "apple-touch-icon", href: "/icons/apple-touch-icon.png" }
        // iOS ignores most of the manifest: these tags give the standalone launch and the title.
        document::Meta { name: "mobile-web-app-capable", content: "yes" }
        document::Meta { name: "apple-mobile-web-app-capable", content: "yes" }
        document::Meta { name: "apple-mobile-web-app-status-bar-style", content: "black" }
        document::Meta { name: "apple-mobile-web-app-title", content: "Iron Oxide" }
        if REGISTER_IN_THIS_BUILD {
            document::Script { "{REGISTER_SERVICE_WORKER}" }
        }
    }
}

/// Consistency checks between the static PWA files in `public/`, the palette and this module.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    fn crate_file(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    /// Path in `public/` for a site-absolute URL such as `/icons/icon-192.png`.
    fn public_file(url: &str) -> PathBuf {
        let relative = url.strip_prefix('/').expect("site-absolute URL");
        crate_file("public").join(relative)
    }

    fn manifest() -> Value {
        let text = std::fs::read_to_string(crate_file("public/manifest.webmanifest")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Width, height and colour type from a PNG's IHDR chunk.
    fn png_header(path: &Path) -> (u32, u32, u8) {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "{} is not a PNG",
            path.display()
        );
        assert_eq!(&bytes[12..16], b"IHDR");
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        (width, height, bytes[25])
    }

    /// PNG colour type 2: RGB without an alpha channel.
    const PNG_RGB: u8 = 2;

    #[test]
    fn manifest_declares_a_standalone_app_at_the_root() {
        let manifest = manifest();
        assert_eq!(manifest["name"], "Iron Oxide");
        assert_eq!(manifest["short_name"], "Fe2O3");
        assert_eq!(manifest["display"], "standalone");
        assert_eq!(manifest["orientation"], "portrait");
        assert_eq!(manifest["start_url"], "/");
        assert_eq!(manifest["scope"], "/");
        assert_eq!(manifest["id"], "/");
    }

    #[test]
    fn manifest_colours_match_the_theme_colour_and_the_palette() {
        let manifest = manifest();
        assert_eq!(manifest["theme_color"], THEME_COLOR);
        assert_eq!(manifest["background_color"], THEME_COLOR);
        let tokens = std::fs::read_to_string(crate_file("assets/tokens.css")).unwrap();
        assert!(tokens.contains(&format!("--io-bg: {THEME_COLOR};")));
    }

    #[test]
    fn manifest_icons_exist_with_their_declared_sizes() {
        let manifest = manifest();
        let icons = manifest["icons"].as_array().unwrap();
        for icon in icons {
            let src = icon["src"].as_str().unwrap();
            let path = public_file(src);
            assert!(path.is_file(), "missing icon {src}");
            if icon["type"] == "image/png" {
                let (width, height, _) = png_header(&path);
                assert_eq!(icon["sizes"], format!("{width}x{height}"), "{src}");
            }
        }
        let has = |size: &str, purpose: &str| {
            icons
                .iter()
                .any(|icon| icon["sizes"] == size && icon["purpose"] == purpose)
        };
        // Chrome's installability criteria need 192 and 512; Android needs a maskable icon.
        assert!(has("192x192", "any"));
        assert!(has("512x512", "any"));
        assert!(has("512x512", "maskable"));
    }

    #[test]
    fn maskable_and_apple_touch_icons_are_opaque() {
        // Both get masked by the OS; transparent pixels would show up as black or white corners.
        let (_, _, maskable) = png_header(&public_file("/icons/icon-maskable-512.png"));
        assert_eq!(maskable, PNG_RGB);
        let (width, height, apple) = png_header(&public_file("/icons/apple-touch-icon.png"));
        assert_eq!((width, height, apple), (180, 180, PNG_RGB));
    }

    #[test]
    fn head_links_point_to_existing_files() {
        for url in [
            "/manifest.webmanifest",
            "/favicon.ico",
            "/icons/favicon.svg",
            "/icons/apple-touch-icon.png",
            "/sw.js",
        ] {
            assert!(public_file(url).is_file(), "missing {url}");
        }
    }

    #[test]
    fn every_service_worker_precache_url_exists() {
        // One missing file makes `cache.addAll` reject, and the worker never installs.
        let sw = std::fs::read_to_string(crate_file("public/sw.js")).unwrap();
        let start = sw.find("const PRECACHE_URLS = [").unwrap();
        let end = start + sw[start..].find("];").unwrap();
        let urls: Vec<&str> = sw[start..end].split('"').skip(1).step_by(2).collect();
        assert!(!urls.is_empty());
        for url in urls {
            assert!(public_file(url).is_file(), "precached {url} is missing");
        }
    }

    #[test]
    fn registration_script_registers_the_root_scoped_worker() {
        assert!(REGISTER_SERVICE_WORKER.contains(r#"register("/sw.js", { scope: "/" })"#));
    }

    #[test]
    fn service_worker_is_registered_in_release_builds_only() {
        assert_eq!(REGISTER_IN_THIS_BUILD, !cfg!(debug_assertions));
    }
}
