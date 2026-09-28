//! Typed server configuration, loaded once at startup from environment variables.
//!
//! Every variable is validated up front: a missing or invalid one stops the server with a message
//! that names the variable (never its value). All problems are reported at once, not one per run.
//! Secrets are wrapped in [`secrecy`] types or redacting newtypes, so `Debug` never prints them.
//!
//! See `.env.example` at the repository root for the full list with placeholders.

use std::{
    env::VarError,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use secrecy::{ExposeSecret, SecretSlice, SecretString};
use sqlx::postgres::PgConnectOptions;
use url::Url;

/// Default bind IP, the same as `dioxus::serve` uses when `IP` is unset.
const DEFAULT_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Default port, the same as `dioxus::serve` uses when `PORT` is unset.
const DEFAULT_PORT: u16 = 8080;
/// Query parameters accepted in `DATABASE_URL`: those sqlx 0.8 understands, plus Neon's
/// `channel_binding` (ignored by sqlx).
const ALLOWED_DATABASE_URL_PARAMS: [&str; 19] = [
    "sslmode",
    "ssl-mode",
    "sslrootcert",
    "ssl-root-cert",
    "ssl-ca",
    "sslcert",
    "ssl-cert",
    "sslkey",
    "ssl-key",
    "statement-cache-capacity",
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "application_name",
    "options",
    "channel_binding",
];
/// Default time in-flight requests get to finish after a shutdown signal. It stays below Fly's
/// `kill_timeout` (30 s in `fly.toml`), leaving room to close the pool before SIGKILL.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(20);
/// Upper bound for `SHUTDOWN_GRACE_SECS`.
const MAX_SHUTDOWN_GRACE_SECS: u64 = 300;
/// A session key must hold at least this many bytes (the size of a `cookie::Key` master key).
pub const SESSION_KEY_MIN_BYTES: usize = 64;

/// Names of the environment variables read by [`Config::from_env`].
pub mod vars {
    pub const DATABASE_URL: &str = "DATABASE_URL";
    pub const APP_BASE_URL: &str = "APP_BASE_URL";
    pub const IP: &str = "IP";
    pub const PORT: &str = "PORT";
    pub const RUST_LOG: &str = "RUST_LOG";
    pub const SHUTDOWN_GRACE_SECS: &str = "SHUTDOWN_GRACE_SECS";
    pub const WEBAUTHN_RP_ID: &str = "WEBAUTHN_RP_ID";
    pub const WEBAUTHN_ORIGIN: &str = "WEBAUTHN_ORIGIN";
    pub const GOOGLE_CLIENT_ID: &str = "GOOGLE_CLIENT_ID";
    pub const GOOGLE_CLIENT_SECRET: &str = "GOOGLE_CLIENT_SECRET";
    pub const GOOGLE_REDIRECT_URL: &str = "GOOGLE_REDIRECT_URL";
    pub const SESSION_KEY: &str = "SESSION_KEY";

    /// The sign-in variables (#5), all required.
    #[cfg(test)]
    pub const AUTH: [&str; 6] = [
        WEBAUTHN_RP_ID,
        WEBAUTHN_ORIGIN,
        GOOGLE_CLIENT_ID,
        GOOGLE_CLIENT_SECRET,
        GOOGLE_REDIRECT_URL,
        SESSION_KEY,
    ];
}

/// The whole server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Postgres connection URL (`DATABASE_URL`). On Neon, the **direct** (non-pooled) endpoint.
    pub database_url: DatabaseUrl,
    /// Public URL the app is served from (`APP_BASE_URL`), e.g. `https://iron-oxide.example`.
    pub app_base_url: Url,
    /// Address the HTTP server binds to (`IP` and `PORT`, the variables `dioxus::serve` reads).
    pub bind_addr: SocketAddr,
    /// Log filter (`RUST_LOG`), already validated. Applied by the Dioxus logger.
    pub log_filter: Option<String>,
    /// How long in-flight requests may run after a shutdown signal (`SHUTDOWN_GRACE_SECS`,
    /// default 20). Keep it a few seconds below the platform's kill timeout.
    pub shutdown_grace: Duration,
    /// Sign-in settings.
    pub auth: AuthConfig,
}

/// The path of the Google OAuth callback route. `GOOGLE_REDIRECT_URL` must be `APP_BASE_URL`'s
/// origin followed by this path.
pub const GOOGLE_CALLBACK_PATH: &str = "/auth/google/callback";

/// Settings for passkeys, Sign in with Google and sessions (#5).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// WebAuthn relying party ID (`WEBAUTHN_RP_ID`): our domain, e.g. `iron-oxide.example`.
    pub webauthn_rp_id: String,
    /// WebAuthn origin (`WEBAUTHN_ORIGIN`), e.g. `https://iron-oxide.example`.
    pub webauthn_origin: Url,
    /// Google OAuth client ID (`GOOGLE_CLIENT_ID`). Public, not a secret.
    pub google_client_id: String,
    /// Google OAuth client secret (`GOOGLE_CLIENT_SECRET`).
    pub google_client_secret: SecretString,
    /// Google OAuth redirect URL (`GOOGLE_REDIRECT_URL`), registered in the Google console.
    pub google_redirect_url: Url,
    /// Key for signing the session cookie (`SESSION_KEY`).
    pub session_key: SessionKey,
    /// Whether cookies get the `Secure` attribute: true unless `APP_BASE_URL` is plain `http` on
    /// a loopback host (local development). Not configurable on its own, so it cannot be turned
    /// off in production.
    pub cookie_secure: bool,
}

/// A Postgres connection URL. It usually embeds a password, so `Debug` only shows the host,
/// port and database name.
#[derive(Clone)]
pub struct DatabaseUrl {
    secret: SecretString,
    redacted: String,
}

impl DatabaseUrl {
    /// Parses and validates a `postgres://` or `postgresql://` URL.
    ///
    /// Error messages never contain the URL or any part of it, as it may hold a password.
    ///
    /// A special character in the password (`/`, `?`, `#`, `@`, ...) must be percent-encoded.
    /// Unencoded, the URL parser ends the host part early and the rest of the password lands in
    /// the path, query or fragment, where it would be treated as a database name or parameter
    /// (and could end up in logs), so such URLs are rejected.
    pub fn parse(raw: &str) -> Result<Self, String> {
        const ENCODE_HINT: &str = "percent-encode special characters in the user name and \
                                   password (e.g. / as %2F, @ as %40, # as %23, ? as %3F)";

        let url = Url::parse(raw).map_err(|error| format!("not a valid URL ({error})"))?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err("the scheme must be postgres:// or postgresql://".to_owned());
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err("the URL has no host".to_owned());
        }
        if url.fragment().is_some() {
            return Err(format!("it has a #fragment; {ENCODE_HINT}"));
        }
        // The path is the database name: one segment, and never an `@` (a sign of a split
        // password).
        let database = url.path().trim_start_matches('/');
        if database.contains('/') || database.contains('@') {
            return Err(format!(
                "the database name part is malformed; {ENCODE_HINT}"
            ));
        }
        // Only the parameters sqlx understands (plus Neon's channel_binding, which sqlx ignores).
        // sqlx logs unknown ones with their key and value, which a split password could be.
        for (key, _) in url.query_pairs() {
            if !ALLOWED_DATABASE_URL_PARAMS.contains(&key.as_ref()) {
                return Err(format!(
                    "it has an unsupported query parameter (allowed: {}); {ENCODE_HINT}",
                    ALLOWED_DATABASE_URL_PARAMS.join(", ")
                ));
            }
        }
        // sqlx has its own parser: check it accepts the URL too, without echoing its error,
        // which could quote the URL.
        let options = PgConnectOptions::from_str(raw)
            .map_err(|_| "sqlx cannot parse it as a Postgres connection URL".to_owned())?;

        // Built from what sqlx will actually connect to, never from the raw string.
        let redacted = format!(
            "{}:{}/{}",
            options.get_host(),
            options.get_port(),
            options.get_database().unwrap_or("(default database)")
        );
        Ok(Self {
            secret: SecretString::from(raw),
            redacted,
        })
    }

    /// Connection options for sqlx.
    pub fn connect_options(&self) -> Result<PgConnectOptions, sqlx::Error> {
        PgConnectOptions::from_str(self.secret.expose_secret())
    }

    /// `host:port/database`, safe to log: no user name, password or parameters.
    pub fn redacted(&self) -> &str {
        &self.redacted
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DatabaseUrl").field(&self.redacted).finish()
    }
}

/// The session cookie key: at least [`SESSION_KEY_MIN_BYTES`] random bytes, given base64-encoded.
#[derive(Clone)]
pub struct SessionKey(SecretSlice<u8>);

impl SessionKey {
    /// Decodes a standard base64 key of at least [`SESSION_KEY_MIN_BYTES`] bytes.
    pub fn parse(raw: &str) -> Result<Self, String> {
        // Line breaks from base64 tools that wrap their output are ignored.
        let compact: String = raw.split_whitespace().collect();
        let bytes = BASE64
            .decode(compact)
            .map_err(|_| "not valid standard base64".to_owned())?;
        if bytes.len() < SESSION_KEY_MIN_BYTES {
            return Err(format!(
                "must decode to at least {SESSION_KEY_MIN_BYTES} bytes, got {}; generate one with \
                 `openssl rand 64 | openssl base64 -A`",
                bytes.len()
            ));
        }
        Ok(Self(SecretSlice::from(bytes)))
    }

    /// The raw key bytes. Only pass them to the cookie/session layer.
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.expose_secret()
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey([redacted])")
    }
}

/// One problem with one variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is not set")]
    Missing { var: &'static str },
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

#[cfg(test)]
impl ConfigError {
    /// The variable this error is about.
    pub fn var(&self) -> &'static str {
        match self {
            Self::Missing { var } | Self::Invalid { var, .. } => var,
        }
    }
}

/// Every problem found while loading the configuration. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigErrors(Vec<ConfigError>);

#[cfg(test)]
impl ConfigErrors {
    pub fn errors(&self) -> &[ConfigError] {
        &self.0
    }
}

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid configuration:")?;
        for error in &self.0 {
            writeln!(f, "  - {error}")?;
        }
        write!(
            f,
            "Set these environment variables (see .env.example for the full list)."
        )
    }
}

impl std::error::Error for ConfigErrors {}

impl Config {
    /// Loads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::from_lookup(|name| std::env::var(name))
    }

    /// Loads the configuration through `lookup`, which behaves like [`std::env::var`].
    ///
    /// A variable set to an empty (or all-whitespace) string counts as unset.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Result<String, VarError>,
    ) -> Result<Self, ConfigErrors> {
        let mut env = Env {
            lookup: &lookup,
            errors: Vec::new(),
        };

        let database_url = env.required(vars::DATABASE_URL, DatabaseUrl::parse);
        let app_base_url = env.required(vars::APP_BASE_URL, parse_http_url);
        let ip = env.optional(vars::IP, |raw| {
            raw.parse::<IpAddr>()
                .map_err(|_| "not an IP address (e.g. 127.0.0.1 or 0.0.0.0)".to_owned())
        });
        let port = env.optional(vars::PORT, parse_port);
        let log_filter = env.optional(vars::RUST_LOG, parse_log_filter);
        let shutdown_grace = env.optional(vars::SHUTDOWN_GRACE_SECS, parse_grace);
        let auth = load_auth(&mut env);
        let auth = match (&app_base_url, auth) {
            (Some(app_base_url), Some(auth)) => {
                check_auth_against_base_url(&mut env, app_base_url, auth)
            }
            _ => None,
        };

        match (database_url, app_base_url, auth, env.errors.is_empty()) {
            (Some(database_url), Some(app_base_url), Some(auth), true) => Ok(Self {
                database_url,
                app_base_url,
                bind_addr: SocketAddr::new(ip.unwrap_or(DEFAULT_IP), port.unwrap_or(DEFAULT_PORT)),
                log_filter,
                shutdown_grace: shutdown_grace.unwrap_or(DEFAULT_SHUTDOWN_GRACE),
                auth,
            }),
            _ => Err(ConfigErrors(env.errors)),
        }
    }
}

/// Loads the sign-in variables. Each one is required.
fn load_auth(env: &mut Env<'_>) -> Option<AuthConfig> {
    let rp_id = env.required(vars::WEBAUTHN_RP_ID, parse_rp_id);
    let origin = env.required(vars::WEBAUTHN_ORIGIN, parse_origin);
    let client_id = env.required(vars::GOOGLE_CLIENT_ID, |raw| Ok(raw.to_owned()));
    let client_secret = env.required(vars::GOOGLE_CLIENT_SECRET, |raw| {
        Ok(SecretString::from(raw))
    });
    let redirect_url = env.required(vars::GOOGLE_REDIRECT_URL, parse_http_url);
    let session_key = env.required(vars::SESSION_KEY, SessionKey::parse);

    let (rp_id, origin) = (rp_id?, origin?);
    if !rp_id_matches_origin(&rp_id, &origin) {
        env.errors.push(ConfigError::Invalid {
            var: vars::WEBAUTHN_RP_ID,
            reason: "must be the host of WEBAUTHN_ORIGIN or a parent domain of it".to_owned(),
        });
        return None;
    }

    Some(AuthConfig {
        webauthn_rp_id: rp_id,
        webauthn_origin: origin,
        google_client_id: client_id?,
        google_client_secret: client_secret?,
        google_redirect_url: redirect_url?,
        session_key: session_key?,
        cookie_secure: true,
    })
}

/// Checks the sign-in settings against `APP_BASE_URL`, the one public origin of the app:
/// - `WEBAUTHN_ORIGIN` must be that origin (the CSRF check also accepts only that origin);
/// - `GOOGLE_REDIRECT_URL` must be that origin plus [`GOOGLE_CALLBACK_PATH`];
/// - plain `http` is only allowed on a loopback host, and only there are cookies not `Secure`.
fn check_auth_against_base_url(
    env: &mut Env<'_>,
    app_base_url: &Url,
    mut auth: AuthConfig,
) -> Option<AuthConfig> {
    let origin = app_base_url.origin();
    let errors_before = env.errors.len();
    if auth.webauthn_origin.origin() != origin {
        env.errors.push(ConfigError::Invalid {
            var: vars::WEBAUTHN_ORIGIN,
            reason: "must be the origin of APP_BASE_URL (same scheme, host and port)".to_owned(),
        });
    }
    let redirect = &auth.google_redirect_url;
    if redirect.origin() != origin
        || redirect.path() != GOOGLE_CALLBACK_PATH
        || redirect.query().is_some()
        || redirect.fragment().is_some()
    {
        env.errors.push(ConfigError::Invalid {
            var: vars::GOOGLE_REDIRECT_URL,
            reason: format!(
                "must be the origin of APP_BASE_URL followed by {GOOGLE_CALLBACK_PATH}"
            ),
        });
    }
    match app_base_url.scheme() {
        "https" => auth.cookie_secure = true,
        _ if is_loopback_host(app_base_url) => auth.cookie_secure = false,
        _ => env.errors.push(ConfigError::Invalid {
            var: vars::APP_BASE_URL,
            reason: "must use https:// (plain http:// is only allowed on localhost)".to_owned(),
        }),
    }
    (env.errors.len() == errors_before).then_some(auth)
}

/// `localhost`, `127.0.0.1` (any 127/8 address) or `[::1]`.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Reads variables and collects every error.
struct Env<'a> {
    lookup: &'a dyn Fn(&str) -> Result<String, VarError>,
    errors: Vec<ConfigError>,
}

enum Raw {
    Unset,
    NotUnicode,
    Value(String),
}

impl Env<'_> {
    fn read(&self, var: &str) -> Raw {
        match (self.lookup)(var) {
            Ok(value) if value.trim().is_empty() => Raw::Unset,
            Ok(value) => Raw::Value(value),
            Err(VarError::NotPresent) => Raw::Unset,
            Err(VarError::NotUnicode(_)) => Raw::NotUnicode,
        }
    }

    /// Whether `var` is set to something (even something invalid).
    fn raw(&self, var: &str) -> Option<()> {
        match self.read(var) {
            Raw::Unset => None,
            Raw::NotUnicode | Raw::Value(_) => Some(()),
        }
    }

    fn optional<T>(
        &mut self,
        var: &'static str,
        parse: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<T> {
        match self.read(var) {
            Raw::Unset => None,
            Raw::NotUnicode => {
                self.errors.push(ConfigError::Invalid {
                    var,
                    reason: "not valid UTF-8".to_owned(),
                });
                None
            }
            Raw::Value(value) => match parse(value.trim()) {
                Ok(parsed) => Some(parsed),
                Err(reason) => {
                    self.errors.push(ConfigError::Invalid { var, reason });
                    None
                }
            },
        }
    }

    fn required<T>(
        &mut self,
        var: &'static str,
        parse: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<T> {
        if self.raw(var).is_none() {
            self.errors.push(ConfigError::Missing { var });
            return None;
        }
        self.optional(var, parse)
    }
}

fn parse_http_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("not a valid URL ({error})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("the scheme must be http:// or https://".to_owned());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("the URL has no host".to_owned());
    }
    Ok(url)
}

/// A WebAuthn origin: scheme, host and optional port, nothing else.
fn parse_origin(raw: &str) -> Result<Url, String> {
    let url = parse_http_url(raw)?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("must be an origin only, without a path, query or fragment".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain credentials".to_owned());
    }
    Ok(url)
}

fn parse_rp_id(raw: &str) -> Result<String, String> {
    const BARE_DOMAIN: &str =
        "must be a bare domain name such as example.com (no scheme, port or path)";
    let rp_id = raw.to_ascii_lowercase();
    if rp_id == "localhost" {
        return Ok(rp_id);
    }
    let labels: Vec<&str> = rp_id.split('.').collect();
    let valid_labels = labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    if !valid_labels {
        return Err(BARE_DOMAIN.to_owned());
    }
    if rp_id.parse::<std::net::IpAddr>().is_ok()
        || labels
            .last()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
    {
        return Err("must be a domain name, not an IP address".to_owned());
    }
    // A single label (`com`, `dev`) is a top-level domain, never our site. Multi-label public
    // suffixes (`co.uk`) are not caught here; browsers reject them as RP IDs anyway.
    if labels.len() < 2 {
        return Err(
            "must be our site's domain (e.g. example.com), not a top-level domain".to_owned(),
        );
    }
    Ok(rp_id)
}

/// The origin's host must equal the RP ID or be a subdomain of it (WebAuthn's rule).
fn rp_id_matches_origin(rp_id: &str, origin: &Url) -> bool {
    origin.host_str().is_some_and(|host| {
        host == rp_id
            || host
                .strip_suffix(rp_id)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn parse_port(raw: &str) -> Result<u16, String> {
    match raw.parse::<u16>() {
        Ok(0) => Err("must be between 1 and 65535".to_owned()),
        Ok(port) => Ok(port),
        Err(_) => Err("must be a number between 1 and 65535".to_owned()),
    }
}

fn parse_grace(raw: &str) -> Result<Duration, String> {
    match raw.parse::<u64>() {
        Ok(secs @ 1..=MAX_SHUTDOWN_GRACE_SECS) => Ok(Duration::from_secs(secs)),
        _ => Err(format!(
            "must be a whole number of seconds between 1 and {MAX_SHUTDOWN_GRACE_SECS}"
        )),
    }
}

fn parse_log_filter(raw: &str) -> Result<String, String> {
    tracing_subscriber::EnvFilter::try_new(raw)
        .map(|_| raw.to_owned())
        .map_err(|error| format!("not a valid tracing filter ({error})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DB: &str = "postgres://iron_oxide:hunter2-db-password@localhost:5433/iron_oxide";
    // 64 zero bytes, base64-encoded.
    const KEY_64: &str =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned().ok_or(VarError::NotPresent))
    }

    /// Every required variable, for local development.
    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            (vars::DATABASE_URL, DB),
            (vars::APP_BASE_URL, "http://localhost:8080"),
            (vars::WEBAUTHN_RP_ID, "localhost"),
            (vars::WEBAUTHN_ORIGIN, "http://localhost:8080"),
            (
                vars::GOOGLE_CLIENT_ID,
                "client-id.apps.googleusercontent.com",
            ),
            (vars::GOOGLE_CLIENT_SECRET, "hunter2-google-secret"),
            (
                vars::GOOGLE_REDIRECT_URL,
                "http://localhost:8080/auth/google/callback",
            ),
            (vars::SESSION_KEY, KEY_64),
        ]
    }

    /// Production-like: https on a real domain.
    fn production() -> Vec<(&'static str, &'static str)> {
        let mut vars = minimal();
        vars.retain(|(k, _)| {
            ![
                vars::APP_BASE_URL,
                vars::WEBAUTHN_RP_ID,
                vars::WEBAUTHN_ORIGIN,
                vars::GOOGLE_REDIRECT_URL,
            ]
            .contains(k)
        });
        vars.extend([
            (vars::APP_BASE_URL, "https://iron-oxyde.com"),
            (vars::WEBAUTHN_RP_ID, "iron-oxyde.com"),
            (vars::WEBAUTHN_ORIGIN, "https://iron-oxyde.com"),
            (
                vars::GOOGLE_REDIRECT_URL,
                "https://iron-oxyde.com/auth/google/callback",
            ),
        ]);
        vars
    }

    fn with_auth() -> Vec<(&'static str, &'static str)> {
        minimal()
    }

    fn set(
        mut vars: Vec<(&'static str, &'static str)>,
        var: &'static str,
        value: &'static str,
    ) -> Vec<(&'static str, &'static str)> {
        vars.retain(|(k, _)| *k != var);
        vars.push((var, value));
        vars
    }

    fn unset(
        mut vars: Vec<(&'static str, &'static str)>,
        var: &str,
    ) -> Vec<(&'static str, &'static str)> {
        vars.retain(|(k, _)| *k != var);
        vars
    }

    fn errors(vars: &[(&str, &str)]) -> Vec<ConfigError> {
        load(vars).unwrap_err().errors().to_vec()
    }

    fn assert_single_invalid(vars: &[(&str, &str)], var: &str) {
        let errors = errors(vars);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            matches!(&errors[0], ConfigError::Invalid { var: v, .. } if *v == var),
            "{errors:?}"
        );
    }

    #[test]
    fn minimal_config_loads_with_defaults() {
        let config = load(&minimal()).unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.app_base_url.as_str(), "http://localhost:8080/");
        assert_eq!(config.log_filter, None);
        assert_eq!(config.shutdown_grace, Duration::from_secs(20));
        assert!(
            !config.auth.cookie_secure,
            "http://localhost is local development"
        );
        assert_eq!(config.database_url.redacted(), "localhost:5433/iron_oxide");
    }

    #[test]
    fn full_config_loads() {
        let vars = set(
            set(set(with_auth(), vars::IP, "0.0.0.0"), vars::PORT, "3000"),
            vars::RUST_LOG,
            "info,sqlx=warn",
        );
        let config = load(&vars).unwrap();
        assert_eq!(config.bind_addr, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(config.log_filter.as_deref(), Some("info,sqlx=warn"));
        let auth = config.auth;
        assert_eq!(auth.webauthn_rp_id, "localhost");
        assert_eq!(auth.webauthn_origin.as_str(), "http://localhost:8080/");
        assert_eq!(
            auth.google_client_id,
            "client-id.apps.googleusercontent.com"
        );
        assert_eq!(
            auth.google_client_secret.expose_secret(),
            "hunter2-google-secret"
        );
        assert_eq!(auth.session_key.expose_bytes(), &[0_u8; 64][..]);
    }

    #[test]
    fn nothing_set_reports_every_required_variable() {
        let errors = errors(&[]);
        let expected: Vec<ConfigError> = [vars::DATABASE_URL, vars::APP_BASE_URL]
            .into_iter()
            .chain(vars::AUTH)
            .map(|var| ConfigError::Missing { var })
            .collect();
        assert_eq!(errors, expected);
    }

    #[test]
    fn missing_database_url_is_named() {
        let errors = errors(&unset(minimal(), vars::DATABASE_URL));
        assert_eq!(
            errors,
            vec![ConfigError::Missing {
                var: vars::DATABASE_URL
            }]
        );
        assert_eq!(errors[0].to_string(), "DATABASE_URL is not set");
    }

    #[test]
    fn empty_or_blank_counts_as_missing() {
        for value in ["", "   "] {
            let errors = errors(&set(minimal(), vars::DATABASE_URL, value));
            assert_eq!(
                errors,
                vec![ConfigError::Missing {
                    var: vars::DATABASE_URL
                }]
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let config = load(&set(minimal(), vars::PORT, " 3000 ")).unwrap();
        assert_eq!(config.bind_addr.port(), 3000);
    }

    #[test]
    fn non_unicode_value_is_invalid() {
        let valid: HashMap<&str, &str> = minimal().into_iter().collect();
        let lookup = |name: &str| match name {
            vars::DATABASE_URL => Err(VarError::NotUnicode(std::ffi::OsString::from("x"))),
            _ => valid
                .get(name)
                .map(|v| (*v).to_owned())
                .ok_or(VarError::NotPresent),
        };
        let errors = Config::from_lookup(lookup).unwrap_err().errors().to_vec();
        assert_eq!(
            errors,
            vec![ConfigError::Invalid {
                var: vars::DATABASE_URL,
                reason: "not valid UTF-8".to_owned()
            }]
        );
    }

    #[test]
    fn invalid_database_urls_are_rejected() {
        for value in [
            "not a url",
            "mysql://user:pw@localhost/db",
            "postgres:///db-without-host",
            "http://localhost:5432/db",
        ] {
            assert_single_invalid(
                &set(minimal(), vars::DATABASE_URL, value),
                vars::DATABASE_URL,
            );
        }
    }

    #[test]
    fn postgresql_scheme_and_neon_parameters_are_accepted() {
        let url = "postgresql://user:pw@ep-x-123.eu-central-1.aws.neon.tech/neondb?sslmode=require&channel_binding=require";
        let config = load(&set(minimal(), vars::DATABASE_URL, url)).unwrap();
        assert_eq!(
            config.database_url.redacted(),
            "ep-x-123.eu-central-1.aws.neon.tech:5432/neondb"
        );
        config.database_url.connect_options().unwrap();
    }

    #[test]
    fn invalid_database_url_error_does_not_echo_the_value() {
        let value = "mysql://user:fake-pw@localhost/db";
        let message = load(&set(minimal(), vars::DATABASE_URL, value))
            .unwrap_err()
            .to_string();
        assert!(message.contains("DATABASE_URL is invalid"), "{message}");
        assert!(!message.contains("fake-pw"), "{message}");
    }

    #[test]
    fn invalid_app_base_urls_are_rejected() {
        for value in ["localhost:8080", "ftp://example.com", "not a url"] {
            assert_single_invalid(
                &set(minimal(), vars::APP_BASE_URL, value),
                vars::APP_BASE_URL,
            );
        }
    }

    #[test]
    fn invalid_ip_is_rejected() {
        assert_single_invalid(&set(minimal(), vars::IP, "localhost"), vars::IP);
    }

    #[test]
    fn port_boundaries() {
        assert_eq!(
            load(&set(minimal(), vars::PORT, "1"))
                .unwrap()
                .bind_addr
                .port(),
            1
        );
        assert_eq!(
            load(&set(minimal(), vars::PORT, "65535"))
                .unwrap()
                .bind_addr
                .port(),
            65535
        );
        for value in ["0", "65536", "-1", "http", "80.5"] {
            assert_single_invalid(&set(minimal(), vars::PORT, value), vars::PORT);
        }
    }

    #[test]
    fn ipv6_bind_address() {
        let config = load(&set(set(minimal(), vars::IP, "::"), vars::PORT, "9000")).unwrap();
        assert_eq!(config.bind_addr, "[::]:9000".parse().unwrap());
    }

    #[test]
    fn shutdown_grace_boundaries() {
        let grace = |value| {
            load(&set(minimal(), vars::SHUTDOWN_GRACE_SECS, value)).map(|c| c.shutdown_grace)
        };
        assert_eq!(grace("1").unwrap(), Duration::from_secs(1));
        assert_eq!(grace("300").unwrap(), Duration::from_secs(300));
        for value in ["0", "301", "-1", "2.5", "20s"] {
            assert_single_invalid(
                &set(minimal(), vars::SHUTDOWN_GRACE_SECS, value),
                vars::SHUTDOWN_GRACE_SECS,
            );
        }
    }

    #[test]
    fn invalid_log_filter_is_rejected() {
        assert_single_invalid(
            &set(minimal(), vars::RUST_LOG, "info,sqlx=loud"),
            vars::RUST_LOG,
        );
    }

    #[test]
    fn all_errors_are_reported_together() {
        let vars = [(vars::PORT, "nope"), (vars::IP, "nope")];
        let found: Vec<&str> = errors(&vars).iter().map(ConfigError::var).collect();
        let expected: Vec<&str> = [vars::DATABASE_URL, vars::APP_BASE_URL, vars::IP, vars::PORT]
            .into_iter()
            .chain(vars::AUTH)
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn production_config_has_secure_cookies() {
        let config = load(&production()).unwrap();
        assert!(config.auth.cookie_secure);
        assert_eq!(config.auth.webauthn_rp_id, "iron-oxyde.com");
    }

    #[test]
    fn plain_http_is_only_allowed_on_loopback_hosts() {
        for base in [
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://LOCALHOST:8080",
        ] {
            let host = Url::parse(base).unwrap().host_str().unwrap().to_owned();
            let host: &'static str = Box::leak(host.into_boxed_str());
            let redirect: &'static str =
                Box::leak(format!("{base}/auth/google/callback").into_boxed_str());
            let vars = set(
                set(
                    set(
                        set(minimal(), vars::APP_BASE_URL, base),
                        vars::WEBAUTHN_ORIGIN,
                        base,
                    ),
                    vars::GOOGLE_REDIRECT_URL,
                    redirect,
                ),
                vars::WEBAUTHN_RP_ID,
                host.trim_start_matches('[').trim_end_matches(']'),
            );
            match load(&vars) {
                Ok(config) => assert!(!config.auth.cookie_secure, "{base}"),
                // WebAuthn RP IDs are domains: an IP address is rejected there, not here.
                Err(errors) => assert!(
                    errors
                        .errors()
                        .iter()
                        .all(|e| e.var() == vars::WEBAUTHN_RP_ID),
                    "{base}: {errors:?}"
                ),
            }
        }

        let vars = set(
            set(
                set(production(), vars::APP_BASE_URL, "http://iron-oxyde.com"),
                vars::WEBAUTHN_ORIGIN,
                "http://iron-oxyde.com",
            ),
            vars::GOOGLE_REDIRECT_URL,
            "http://iron-oxyde.com/auth/google/callback",
        );
        assert_single_invalid(&vars, vars::APP_BASE_URL);
    }

    #[test]
    fn webauthn_origin_must_be_the_app_origin() {
        for origin in [
            "http://localhost:3000",
            "https://localhost:8080",
            "http://sub.localhost:8080",
        ] {
            assert_single_invalid(
                &set(minimal(), vars::WEBAUTHN_ORIGIN, origin),
                vars::WEBAUTHN_ORIGIN,
            );
        }
    }

    #[test]
    fn google_redirect_url_must_be_the_app_callback() {
        for url in [
            "http://localhost:8080/auth/google/other",
            "http://localhost:3000/auth/google/callback",
            "https://localhost:8080/auth/google/callback",
            "http://evil.example/auth/google/callback",
            "http://localhost:8080/auth/google/callback?x=1",
            "http://localhost:8080/auth/google/callback#f",
        ] {
            assert_single_invalid(
                &set(minimal(), vars::GOOGLE_REDIRECT_URL, url),
                vars::GOOGLE_REDIRECT_URL,
            );
        }
    }

    #[test]
    fn each_missing_auth_variable_is_reported() {
        for var in vars::AUTH {
            let errors = errors(&unset(with_auth(), var));
            assert_eq!(errors, vec![ConfigError::Missing { var }]);
        }
    }

    #[test]
    fn short_session_key_is_rejected() {
        // 63 bytes.
        let key =
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let errors = errors(&set(with_auth(), vars::SESSION_KEY, key));
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].to_string().contains("at least 64 bytes, got 63"),
            "{}",
            errors[0]
        );
    }

    #[test]
    fn wrapped_session_key_is_accepted() {
        let wrapped = format!("{}\n{}", &KEY_64[..64], &KEY_64[64..]);
        let key = SessionKey::parse(&wrapped).unwrap();
        assert_eq!(key.expose_bytes(), &[0_u8; 64][..]);
        assert_eq!(format!("{key:?}"), "SessionKey([redacted])");
    }

    #[test]
    fn non_base64_session_key_is_rejected() {
        assert_single_invalid(
            &set(with_auth(), vars::SESSION_KEY, "not base64!"),
            vars::SESSION_KEY,
        );
    }

    #[test]
    fn webauthn_origin_must_be_an_origin() {
        for value in [
            "http://localhost:8080/path",
            "http://localhost:8080?q=1",
            "http://u:p@localhost",
            "localhost",
        ] {
            assert_single_invalid(
                &set(with_auth(), vars::WEBAUTHN_ORIGIN, value),
                vars::WEBAUTHN_ORIGIN,
            );
        }
    }

    #[test]
    fn rp_id_must_not_be_a_top_level_domain_or_an_ip() {
        for (rp_id, origin) in [
            ("com", "https://app.example.com"),
            ("dev", "https://example.dev"),
            ("127.0.0.1", "https://127.0.0.1"),
            ("10.0.0.1", "http://10.0.0.1:8080"),
        ] {
            let vars = set(
                set(with_auth(), vars::WEBAUTHN_RP_ID, rp_id),
                vars::WEBAUTHN_ORIGIN,
                origin,
            );
            assert_single_invalid(&vars, vars::WEBAUTHN_RP_ID);
        }
    }

    #[test]
    fn rp_id_must_be_a_bare_domain() {
        for value in [
            "a..b",
            "-a.com",
            "a-.com",
            "a_b.com",
            "https://localhost",
            "localhost:8080",
            ".example.com",
            "example.com.",
            "a/b",
        ] {
            assert_single_invalid(
                &set(with_auth(), vars::WEBAUTHN_RP_ID, value),
                vars::WEBAUTHN_RP_ID,
            );
        }
    }

    #[test]
    fn rp_id_may_be_a_parent_domain_of_the_origin() {
        let vars = set(
            set(
                set(
                    set(production(), vars::WEBAUTHN_RP_ID, "Example.com"),
                    vars::WEBAUTHN_ORIGIN,
                    "https://app.example.com",
                ),
                vars::APP_BASE_URL,
                "https://app.example.com",
            ),
            vars::GOOGLE_REDIRECT_URL,
            "https://app.example.com/auth/google/callback",
        );
        assert_eq!(load(&vars).unwrap().auth.webauthn_rp_id, "example.com");
    }

    #[test]
    fn rp_id_must_match_the_origin() {
        for (rp_id, origin) in [
            ("example.com", "https://notexample.com"),
            ("app.example.com", "https://example.com"),
            ("example.org", "https://example.com"),
        ] {
            let vars = set(
                set(with_auth(), vars::WEBAUTHN_RP_ID, rp_id),
                vars::WEBAUTHN_ORIGIN,
                origin,
            );
            assert_single_invalid(&vars, vars::WEBAUTHN_RP_ID);
        }
    }

    #[test]
    fn invalid_google_redirect_url_is_rejected() {
        assert_single_invalid(
            &set(with_auth(), vars::GOOGLE_REDIRECT_URL, "/relative"),
            vars::GOOGLE_REDIRECT_URL,
        );
    }

    /// Passwords with characters that break naive URL handling, each tried raw (as people
    /// paste them) and percent-encoded (as they should be). Each holds the marker `SECRETxyz`,
    /// which must never be shown.
    const NASTY_PASSWORDS: [&str; 11] = [
        "/SECRETxyz",
        "2024/SECRETxyz",
        "SECRETxyz@at",
        "@SECRETxyz",
        "SECRETxyz:colon",
        "SECRETxyz#hash",
        "SECRETxyz?query",
        "2024?SECRETxyz",
        "2024#SECRETxyz",
        "SECRETxyz%41pct",
        "/a@b:c#d?SECRETxyz",
    ];

    const MARKER: &str = "SECRETxyz";

    fn percent_encode(password: &str) -> String {
        url::form_urlencoded::byte_serialize(password.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    }

    fn assert_nothing_leaks(url: &str) {
        let secret = MARKER;
        match DatabaseUrl::parse(url) {
            Ok(parsed) => {
                let shown = format!("{} {parsed:?}", parsed.redacted());
                assert!(!shown.contains(secret), "{url} -> {shown}");
                assert!(!shown.contains('@'), "{url} -> {shown}");
                // What sqlx will connect to must be the real host and database.
                let options = parsed.connect_options().unwrap();
                assert_eq!(options.get_host(), "db.example.com", "{url}");
                assert_eq!(options.get_database(), Some("appdb"), "{url}");
            }
            Err(reason) => assert!(!reason.contains(secret), "{url} -> {reason}"),
        }
        // Through the whole config too: its Debug and its error Display.
        let result = load(&[
            (vars::DATABASE_URL, url),
            (vars::APP_BASE_URL, "http://localhost"),
        ]);
        let shown = match result {
            Ok(config) => format!("{config:?}"),
            Err(errors) => format!("{errors} {errors:?}"),
        };
        assert!(!shown.contains(secret), "{url} -> {shown}");
    }

    #[test]
    fn nasty_passwords_never_leak_raw_or_encoded() {
        for password in NASTY_PASSWORDS {
            for user in ["appuser", "", "2024"] {
                let raw = format!("postgres://{user}:{password}@db.example.com:5432/appdb");
                assert_nothing_leaks(&raw);
                let encoded = format!(
                    "postgres://{user}:{}@db.example.com:5432/appdb?sslmode=require",
                    percent_encode(password)
                );
                assert_nothing_leaks(&encoded);
                // Encoded passwords are valid and must be accepted.
                assert!(DatabaseUrl::parse(&encoded).is_ok(), "{encoded}");
            }
        }
    }

    #[test]
    fn a_raw_slash_in_the_password_is_rejected() {
        // The URL parser splits these at the `/`: the host becomes the user name and the rest of
        // the password lands in the path.
        for url in [
            "postgres://u:/PWslash9@127.0.0.1:5461/leakdb",
            "postgres://u:2024/PWslash9@127.0.0.1:5461/leakdb",
        ] {
            let reason = DatabaseUrl::parse(url).unwrap_err();
            assert!(reason.contains("percent-encode"), "{reason}");
            assert!(!reason.contains("PWslash9"), "{reason}");
        }
    }

    #[test]
    fn unknown_database_url_parameters_are_rejected_without_naming_them() {
        let reason =
            DatabaseUrl::parse("postgres://u@db.example.com/appdb?PWsecret9x=1").unwrap_err();
        assert!(reason.contains("unsupported query parameter"), "{reason}");
        assert!(!reason.contains("PWsecret9x"), "{reason}");
    }

    #[test]
    fn password_query_parameter_is_accepted_and_hidden() {
        let parsed =
            DatabaseUrl::parse("postgres://db.example.com/appdb?user=u&password=PWparam9x")
                .unwrap();
        assert_eq!(parsed.redacted(), "db.example.com:5432/appdb");
        assert!(!format!("{parsed:?}").contains("PWparam9x"));
    }

    #[test]
    fn debug_redacts_every_secret() {
        let config = load(&with_auth()).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains(KEY_64), "{debug}");
        assert!(!debug.contains("AAAA"), "{debug}");
        // Non-secret values stay visible for troubleshooting.
        assert!(debug.contains("localhost:5433/iron_oxide"), "{debug}");
        assert!(
            debug.contains("client-id.apps.googleusercontent.com"),
            "{debug}"
        );
    }

    #[test]
    fn display_lists_every_error_and_points_to_env_example() {
        let message = load(&unset(minimal(), vars::DATABASE_URL))
            .unwrap_err()
            .to_string();
        assert_eq!(
            message,
            "invalid configuration:\n  - DATABASE_URL is not set\n\
             Set these environment variables (see .env.example for the full list)."
        );
    }
}
