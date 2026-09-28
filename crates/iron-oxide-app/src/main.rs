//! Iron Oxide: a Dioxus fullstack app.
//!
//! The same crate builds twice: the browser client (`web` feature, wasm32) and the axum server
//! (`server` feature), which serves the client, the server functions and custom routes.

// The Wake Lock API needs web-sys unstable APIs; see .cargo/config.toml.
#[cfg(all(target_arch = "wasm32", not(web_sys_unstable_apis)))]
compile_error!("wasm builds need `--cfg=web_sys_unstable_apis` (set in .cargo/config.toml)");

mod api;
mod auth;
#[cfg(feature = "server")]
mod server;
mod ui;

#[cfg(not(feature = "server"))]
fn main() {
    dioxus::launch(ui::App);
}

/// Loads the configuration and runs the server. A bad configuration exits with status 1 and a
/// message naming each missing or invalid variable, before anything else starts.
#[cfg(feature = "server")]
fn main() -> std::process::ExitCode {
    // A local `.env` in the working directory is optional; real environment variables win.
    if let Err(error) = server::dotenv::load(std::path::Path::new(".env")) {
        eprintln!("error: {error}");
        return std::process::ExitCode::FAILURE;
    }

    match server::Config::from_env() {
        Ok(config) => server::serve(config),
        Err(errors) => {
            eprintln!("error: {errors}");
            std::process::ExitCode::FAILURE
        }
    }
}
