//! The server's logger: the same output as `dioxus::logger::initialize_default`, with a filter
//! that keeps sign-in material out of the logs at every `RUST_LOG` level.
//!
//! `webauthn-rs-core` logs credential ids and public keys at `debug`, and the challenge and the
//! whole registration at `trace`. Those targets are capped at `info` whatever `RUST_LOG` says:
//! any directive for them (or scoped to a span, which could re-enable them) is dropped before
//! ours are appended.

use dioxus::logger::tracing;
use tracing_subscriber::EnvFilter;

/// Targets that must never log below `info`.
const CAPPED_TARGETS: [&str; 2] = ["webauthn_rs_core", "webauthn_rs"];

/// Builds the filter directives: the user's (`RUST_LOG`, already validated) or the default
/// level, minus anything that could lower the cap, plus the caps.
#[must_use]
pub fn filter_directives(user: Option<&str>) -> String {
    let default = if cfg!(debug_assertions) {
        "debug"
    } else {
        "info"
    };
    let user = user.unwrap_or(default);
    let mut directives: Vec<String> = user
        .split(',')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .filter(|directive| !could_lower_the_cap(directive))
        .map(str::to_owned)
        .collect();
    // hyper has spammy `debug!` calls, as in `dioxus::logger`.
    directives.push("hyper_util=warn".to_owned());
    directives.extend(CAPPED_TARGETS.iter().map(|target| format!("{target}=info")));
    directives.join(",")
}

/// A span-scoped directive, or one whose target is (inside) a capped crate.
fn could_lower_the_cap(directive: &str) -> bool {
    if directive.starts_with('[') {
        return true;
    }
    let target = directive
        .split(['[', '='])
        .next()
        .unwrap_or(directive)
        .trim();
    CAPPED_TARGETS
        .iter()
        .any(|capped| target == *capped || target.starts_with(&format!("{capped}::")))
}

/// The filter for [`filter_directives`].
pub fn env_filter(user: Option<&str>) -> Result<EnvFilter, String> {
    EnvFilter::try_new(filter_directives(user)).map_err(|error| error.to_string())
}

/// Installs the global logger. Does nothing if one is already set (tests).
pub fn init(user: Option<&str>) {
    if tracing::dispatcher::has_been_set() {
        return;
    }
    match env_filter(user) {
        Ok(filter) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
        Err(error) => {
            // RUST_LOG was validated at startup; fall back to the Dioxus default.
            eprintln!("warning: cannot build the log filter ({error}); using the default");
            dioxus::logger::initialize_default();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Logs a few events under the filter built from `rust_log`; returns what was written.
    fn logged(rust_log: &str) -> String {
        let out = Captured::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(env_filter(Some(rust_log)).unwrap())
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "webauthn_rs_core::core", "SECRET-trace-state");
            tracing::debug!(target: "webauthn_rs_core::core", "SECRET-debug-credential");
            tracing::debug!(target: "webauthn_rs", "SECRET-debug-webauthn-rs");
            tracing::info!(target: "webauthn_rs_core::core", "info-is-fine");
            tracing::trace!(target: "iron_oxide_app::server", "app-trace");
            let span = tracing::info_span!("x");
            let _entered = span.enter();
            tracing::trace!(target: "webauthn_rs_core::core", "SECRET-in-span");
        });
        let bytes = out.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn webauthn_stays_at_info_even_when_everything_is_traced() {
        for rust_log in [
            "trace",
            "webauthn_rs_core=trace",
            "webauthn_rs_core::core=trace,webauthn_rs=debug",
            "trace,[x]=trace,[{a}]=trace",
            "info,webauthn_rs_core[x]=trace",
        ] {
            let logs = logged(rust_log);
            assert!(!logs.contains("SECRET"), "{rust_log}: {logs}");
            assert!(logs.contains("info-is-fine"), "{rust_log}: {logs}");
        }
        assert!(logged("trace").contains("app-trace"));
    }

    #[test]
    fn other_directives_are_kept() {
        let directives = filter_directives(Some("info, sqlx=warn ,webauthn_rs_core=trace"));
        assert_eq!(
            directives,
            "info,sqlx=warn,hyper_util=warn,webauthn_rs_core=info,webauthn_rs=info"
        );
        assert!(filter_directives(None).ends_with("webauthn_rs_core=info,webauthn_rs=info"));
    }

    #[test]
    fn only_capped_targets_are_dropped() {
        assert!(could_lower_the_cap("webauthn_rs_core"));
        assert!(could_lower_the_cap("webauthn_rs=trace"));
        assert!(could_lower_the_cap("webauthn_rs_core::core=debug"));
        assert!(could_lower_the_cap("[span]=trace"));
        assert!(!could_lower_the_cap("webauthn_rs_proto=trace"));
        assert!(!could_lower_the_cap("iron_oxide_app=trace"));
        assert!(!could_lower_the_cap("trace"));
    }
}
