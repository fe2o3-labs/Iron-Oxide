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

#[cfg(not(feature = "server"))]
fn main() {
    dioxus::launch(ui::App);
}

/// Loads the configuration and starts the server. A bad configuration exits with status 1 and a
/// message naming each missing or invalid variable, before anything else starts.
#[cfg(feature = "server")]
fn main() {
    // A local `.env` is optional (production sets real environment variables, which win).
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(error) if error.not_found() => {}
        Err(error) => {
            eprintln!("error: cannot read the .env file: {error}");
            std::process::exit(1);
        }
    }

    match server::Config::from_env() {
        Ok(config) => server::serve(config),
        Err(errors) => {
            eprintln!("error: {errors}");
            std::process::exit(1);
        }
    }
}
