//! Iron Oxide: a Dioxus fullstack app.
//!
//! The same crate builds twice: the browser client (`web` feature, wasm32) and the axum server
//! (`server` feature), which serves the client, the server functions and custom routes.

// The Wake Lock API needs web-sys unstable APIs; see .cargo/config.toml.
#[cfg(all(target_arch = "wasm32", not(web_sys_unstable_apis)))]
compile_error!("wasm builds need `--cfg=web_sys_unstable_apis` (set in .cargo/config.toml)");

mod api;
#[cfg(feature = "server")]
mod server;
mod ui;

fn main() {
    #[cfg(not(feature = "server"))]
    dioxus::launch(ui::App);

    // `dioxus::serve` binds to the `IP` and `PORT` environment variables (set by `dx serve`).
    #[cfg(feature = "server")]
    dioxus::serve(|| async { Ok(server::router()) });
}
