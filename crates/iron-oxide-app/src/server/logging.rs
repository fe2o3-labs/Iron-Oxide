//! The server's logger: the same output as `dioxus::logger::initialize_default`, with sign-in
//! material kept out of the logs at every `RUST_LOG` level.
//!
//! `webauthn-rs-core` logs credential ids and public keys at `debug`, and the challenge and the
//! whole registration at `trace`. A per-event filter drops every event and span whose target is
//! in `webauthn_rs*` below `INFO`, after and independently of the `RUST_LOG` filter, so no
//! directive (span-scoped, prefixed, or otherwise) can let them through.

use dioxus::logger::tracing::{self, Level, Metadata};
use tracing_subscriber::{
    EnvFilter, Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt,
};

/// Targets under this prefix never log below `INFO` (`webauthn_rs`, `webauthn_rs_core`, …).
const CAPPED_PREFIX: &str = "webauthn_rs";

/// The `RUST_LOG` filter (already validated at startup) or the default level, plus the Dioxus
/// logger's `hyper_util=warn`.
pub fn env_filter(user: Option<&str>) -> Result<EnvFilter, String> {
    let default = if cfg!(debug_assertions) {
        "debug"
    } else {
        "info"
    };
    let directives = format!("{},hyper_util=warn", user.unwrap_or(default));
    EnvFilter::try_new(directives).map_err(|error| error.to_string())
}

/// Whether the event or span may be logged, whatever the `RUST_LOG` filter says.
#[must_use]
pub fn allowed(metadata: &Metadata<'_>) -> bool {
    !(metadata.target().starts_with(CAPPED_PREFIX) && *metadata.level() > Level::INFO)
}

/// Installs the global logger. Does nothing if one is already set (tests).
pub fn init(user: Option<&str>) {
    if tracing::dispatcher::has_been_set() {
        return;
    }
    let filter = match env_filter(user) {
        Ok(filter) => filter,
        Err(error) => {
            // RUST_LOG was validated at startup; fall back to the default level.
            eprintln!("warning: cannot build the log filter ({error}); using the default");
            EnvFilter::new("info")
        }
    };
    let layer = tracing_subscriber::fmt::layer()
        .with_filter(filter)
        .with_filter(filter_fn(allowed));
    let _ = tracing_subscriber::registry().with(layer).try_init();
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

    /// Logs a few events through the same layers as [`init`], with `rust_log`; returns the
    /// output.
    fn logged(rust_log: &str) -> String {
        let out = Captured::default();
        let writer = out.clone();
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_filter(env_filter(Some(rust_log)).unwrap())
            .with_filter(filter_fn(allowed));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "webauthn_rs_core::core", "SECRET-trace-state");
            tracing::debug!(target: "webauthn_rs_core::core", "SECRET-debug-credential");
            tracing::debug!(target: "webauthn_rs", "SECRET-debug-webauthn-rs");
            tracing::info!(target: "webauthn_rs_core::core", "info-is-fine");
            tracing::trace!(target: "iron_oxide_app::server", "app-trace");
            let span = tracing::info_span!(target: "tower_sessions", "call");
            let _entered = span.enter();
            tracing::trace!(target: "webauthn_rs_core::core", "SECRET-in-span");
            tracing::trace!(target: "webauthn_rs_core:", "SECRET-odd-target");
        });
        let bytes = out.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn webauthn_stays_at_info_whatever_rust_log_says() {
        for rust_log in [
            "trace",
            "webauthn_rs_core=trace",
            "webauthn_rs_core::core=trace,webauthn_rs=debug",
            "trace,[x]=trace,[{a}]=trace",
            "info,webauthn_rs_core[x]=trace",
            // From the second security review.
            "tower_sessions[call]=trace",
            "webauthn_rs_core:=trace",
        ] {
            let logs = logged(rust_log);
            assert!(!logs.contains("SECRET"), "{rust_log}: {logs}");
        }
        let logs = logged("trace");
        assert!(logs.contains("info-is-fine"), "{logs}");
        assert!(logs.contains("app-trace"), "{logs}");
    }

    #[test]
    fn only_webauthn_below_info_is_dropped() {
        let meta = |target: &'static str, level: Level| {
            let callsite = tracing::callsite::Identifier(&CALLSITE);
            Metadata::new(
                "event",
                target,
                level,
                None,
                None,
                None,
                tracing::field::FieldSet::new(&[], callsite),
                tracing::metadata::Kind::EVENT,
            )
        };
        assert!(!allowed(&meta("webauthn_rs_core::core", Level::DEBUG)));
        assert!(!allowed(&meta("webauthn_rs", Level::TRACE)));
        assert!(!allowed(&meta("webauthn_rs_proto", Level::DEBUG)));
        assert!(allowed(&meta("webauthn_rs_core", Level::INFO)));
        assert!(allowed(&meta("webauthn_rs_core", Level::WARN)));
        assert!(allowed(&meta("iron_oxide_app", Level::TRACE)));
        assert!(allowed(&meta("tower_sessions", Level::TRACE)));
    }

    struct Callsite;
    static CALLSITE: Callsite = Callsite;
    impl tracing::callsite::Callsite for Callsite {
        fn set_interest(&self, _: tracing::subscriber::Interest) {}
        fn metadata(&self) -> &Metadata<'_> {
            unimplemented!()
        }
    }

    #[test]
    fn env_filter_accepts_the_default_and_user_filters() {
        assert!(env_filter(None).is_ok());
        assert!(env_filter(Some("info,sqlx=warn")).is_ok());
    }
}
