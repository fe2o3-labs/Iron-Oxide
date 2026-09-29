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

use super::rate_limit::{ClientIpSource, RateLimitConfig};

/// Default bind IP, the same as `dioxus::serve` uses when `IP` is unset.
const DEFAULT_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Default port, the same as `dioxus::serve` uses when `PORT` is unset.
const DEFAULT_PORT: u16 = 8080;
/// Query parameters accepted in `DATABASE_URL` and passed to sqlx 0.8, which understands them.
/// `host`, `hostaddr` and `port` are not accepted: the URL names the one host we connect to.
const ALLOWED_DATABASE_URL_PARAMS: [&str; 15] = [
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
    "dbname",
    "user",
    "password",
    "application_name",
    // Neon uses it for the endpoint ID (`options=endpoint%3D...`).
    "options",
];

/// Parameters Neon documents that sqlx does not support: accepted, then removed before the URL
/// reaches sqlx (which would log a warning with their value on every start).
/// - `channel_binding`: sqlx does not do SCRAM channel binding; TLS still applies via `sslmode`.
/// - `connect_timeout`: the app bounds each connection attempt itself (see `db::RetryPolicy`).
/// - `sslnegotiation`: sqlx always uses the standard `SSLRequest` negotiation.
const STRIPPED_DATABASE_URL_PARAMS: [&str; 3] =
    ["channel_binding", "connect_timeout", "sslnegotiation"];

/// The error for a query parameter we do not accept. The name is shown when it looks like a
/// parameter name (it cannot be part of a password then: a split password always brings an `@`,
/// rejected earlier).
fn unsupported_param(key: &str) -> String {
    let looks_like_a_name = !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    let name = if looks_like_a_name {
        format!("`{key}`")
    } else {
        "(name not shown)".to_owned()
    };
    format!(
        "unsupported parameter {name}; allowed: {}, {}",
        ALLOWED_DATABASE_URL_PARAMS.join(", "),
        STRIPPED_DATABASE_URL_PARAMS.join(", ")
    )
}

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
    pub const CLIENT_IP_SOURCE: &str = "CLIENT_IP_SOURCE";
    pub const STRIPE_WEBHOOK_SECRET: &str = "STRIPE_WEBHOOK_SECRET";

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
    /// Rate limits (#23): where client IPs come from (`CLIENT_IP_SOURCE`, default `peer`) and
    /// the limits themselves (built in, see `docs/rate-limiting.md`).
    pub rate_limit: RateLimitConfig,
    /// Billing settings (#21, see docs/billing.md).
    pub billing: BillingConfig,
}

/// Billing settings (#21). All optional while billing is not implemented.
#[derive(Debug, Clone, Default)]
pub struct BillingConfig {
    /// The Stripe webhook endpoint's signing secret (`STRIPE_WEBHOOK_SECRET`), used to verify
    /// `Stripe-Signature`. Optional: the webhook is a stub that does not use it yet. Once it is
    /// implemented, the webhook refuses every event while it is unset.
    pub stripe_webhook_secret: Option<SecretString>,
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
        // The path is the database name: one segment, and never an `@` (a sign of a split
        // password).
        let database = url.path().trim_start_matches('/');
        if database.contains('/') || database.contains('@') {
            return Err(format!(
                "the database name part is malformed; {ENCODE_HINT}"
            ));
        }
        // A password split at `?` always leaves its `@host` in the query. A real `@` in a
        // parameter value must be written %40.
        if url.query().is_some_and(|query| query.contains('@')) {
            return Err(format!("the query string contains a raw @; {ENCODE_HINT}"));
        }

        // Only parameters we expect. sqlx logs unknown ones with their value, and `host`,
        // `hostaddr` and `port` would redirect the connection away from the URL's host.
        let mut kept = Vec::new();
        for (key, value) in url.query_pairs() {
            if STRIPPED_DATABASE_URL_PARAMS.contains(&key.as_ref()) {
                continue;
            }
            if !ALLOWED_DATABASE_URL_PARAMS.contains(&key.as_ref()) {
                return Err(unsupported_param(&key));
            }
            kept.push((key.into_owned(), value.into_owned()));
        }
        // Hand sqlx the URL without the parameters it would only warn about.
        let mut sanitized = url.clone();
        if kept.is_empty() {
            sanitized.set_query(None);
        } else {
            sanitized.query_pairs_mut().clear().extend_pairs(&kept);
        }

        // sqlx has its own parser: check it accepts the URL too, without echoing its error,
        // which could quote the URL.
        let options = PgConnectOptions::from_str(sanitized.as_str())
            .map_err(|_| "sqlx cannot parse it as a Postgres connection URL".to_owned())?;

        // Built from what sqlx will actually connect to, never from the raw string.
        let host = match options.get_socket() {
            Some(socket) => socket.display().to_string(),
            None => format!("{}:{}", options.get_host(), options.get_port()),
        };
        let redacted = format!(
            "{host}/{}",
            options.get_database().unwrap_or("(default database)")
        );
        Ok(Self {
            secret: SecretString::from(sanitized.as_str()),
            redacted,
        })
    }

    /// Connection options for sqlx.
    pub fn connect_options(&self) -> Result<PgConnectOptions, sqlx::Error> {
        PgConnectOptions::from_str(self.secret.expose_secret())
    }

    /// `host:port/database` (or `socket/database`), safe to log: no user name, password or
    /// parameters.
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
        let client_ip = env.optional(vars::CLIENT_IP_SOURCE, str::parse::<ClientIpSource>);
        let auth = load_auth(&mut env);
        let billing = BillingConfig {
            stripe_webhook_secret: env.optional(vars::STRIPE_WEBHOOK_SECRET, |raw| {
                Ok(SecretString::from(raw))
            }),
        };
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
                rate_limit: RateLimitConfig {
                    client_ip: client_ip.unwrap_or_default(),
                    ..RateLimitConfig::default()
                },
                billing,
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
    let redirect_url = env.required(vars::GOOGLE_REDIRECT_URL, parse_secure_url);
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

/// An http(s) URL that must be https unless its host is the local machine (`localhost`,
/// `127.0.0.1`, `[::1]`): WebAuthn and Google sign-in need a secure context elsewhere.
fn parse_secure_url(raw: &str) -> Result<Url, String> {
    let url = parse_http_url(raw)?;
    let local = matches!(url.host(), Some(url::Host::Domain("localhost")))
        || matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    if url.scheme() != "https" && !local {
        return Err("must use https:// (http:// is only allowed for localhost)".to_owned());
    }
    Ok(url)
}

/// A WebAuthn origin: scheme, host and optional port, nothing else.
fn parse_origin(raw: &str) -> Result<Url, String> {
    let url = parse_secure_url(raw)?;
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
        assert_eq!(config.rate_limit, RateLimitConfig::default());
        assert_eq!(config.rate_limit.client_ip, ClientIpSource::Peer);
    }

    #[test]
    fn client_ip_source_is_peer_unless_set_to_fly() {
        let fly = load(&set(minimal(), vars::CLIENT_IP_SOURCE, "fly")).unwrap();
        assert_eq!(fly.rate_limit.client_ip, ClientIpSource::Fly);
        let peer = load(&set(minimal(), vars::CLIENT_IP_SOURCE, " peer ")).unwrap();
        assert_eq!(peer.rate_limit.client_ip, ClientIpSource::Peer);
        let blank = load(&set(minimal(), vars::CLIENT_IP_SOURCE, " ")).unwrap();
        assert_eq!(blank.rate_limit.client_ip, ClientIpSource::Peer);
    }

    #[test]
    fn an_unknown_client_ip_source_is_rejected() {
        for value in ["x-forwarded-for", "true", "1"] {
            assert_single_invalid(
                &set(minimal(), vars::CLIENT_IP_SOURCE, value),
                vars::CLIENT_IP_SOURCE,
            );
        }
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

        // Only APP_BASE_URL on plain http (the sign-in URLs have their own https rule).
        let vars = set(production(), vars::APP_BASE_URL, "http://iron-oxyde.com");
        let found: Vec<&str> = errors(&vars).iter().map(ConfigError::var).collect();
        assert!(found.contains(&vars::APP_BASE_URL), "{found:?}");
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
            ("10.0.0.1", "https://10.0.0.1:8080"),
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
    fn the_production_app_subdomain_with_the_parent_rp_id_is_accepted() {
        // The app at app.iron-oxyde.com, passkeys bound to the parent domain (#7, #70).
        let vars = set(
            set(
                set(
                    set(
                        production(),
                        vars::APP_BASE_URL,
                        "https://app.iron-oxyde.com",
                    ),
                    vars::WEBAUTHN_ORIGIN,
                    "https://app.iron-oxyde.com",
                ),
                vars::WEBAUTHN_RP_ID,
                "iron-oxyde.com",
            ),
            vars::GOOGLE_REDIRECT_URL,
            "https://app.iron-oxyde.com/auth/google/callback",
        );
        let config = load(&vars).unwrap();
        assert_eq!(config.auth.webauthn_rp_id, "iron-oxyde.com");
        assert_eq!(
            config.auth.webauthn_origin.as_str(),
            "https://app.iron-oxyde.com/"
        );
        assert!(config.auth.cookie_secure);
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
    fn plain_http_is_only_allowed_for_localhost() {
        // The sign-in URLs must share APP_BASE_URL's origin (#5), so all three move together.
        fn served_at(
            origin: &'static str,
            rp_id: &'static str,
        ) -> Vec<(&'static str, &'static str)> {
            let redirect: &'static str =
                Box::leak(format!("{origin}/auth/google/callback").into_boxed_str());
            let vars = set(minimal(), vars::APP_BASE_URL, origin);
            let vars = set(vars, vars::WEBAUTHN_ORIGIN, origin);
            let vars = set(vars, vars::GOOGLE_REDIRECT_URL, redirect);
            set(vars, vars::WEBAUTHN_RP_ID, rp_id)
        }
        for origin in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            let host = Url::parse(origin).unwrap().host_str().unwrap().to_owned();
            let rp_id = if host == "localhost" {
                "localhost"
            } else {
                "example.com"
            };
            let found = errors_or_ok(&served_at(origin, rp_id));
            assert!(
                found
                    .iter()
                    .all(|e| e.var() != vars::WEBAUTHN_ORIGIN
                        && e.var() != vars::GOOGLE_REDIRECT_URL),
                "{origin}: {found:?}"
            );
        }
        let found: Vec<&str> = errors_or_ok(&served_at(
            "http://iron-oxide.example",
            "iron-oxide.example",
        ))
        .iter()
        .map(ConfigError::var)
        .collect();
        assert_eq!(found, [vars::WEBAUTHN_ORIGIN, vars::GOOGLE_REDIRECT_URL]);
        let vars = set(
            minimal(),
            vars::GOOGLE_REDIRECT_URL,
            "http://iron-oxide.example/auth/google/callback",
        );
        assert_single_invalid(&vars, vars::GOOGLE_REDIRECT_URL);
        load(&served_at(
            "https://iron-oxide.example",
            "iron-oxide.example",
        ))
        .unwrap();
    }

    fn errors_or_ok(vars: &[(&str, &str)]) -> Vec<ConfigError> {
        load(vars)
            .err()
            .map(|e| e.errors().to_vec())
            .unwrap_or_default()
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
    const NASTY_PASSWORDS: [&str; 19] = [
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
        // Second review: split at `?` or `/` so the tail lands in an allowed parameter.
        "12?host=SECRETxyz",
        "?dbname=SECRETxyz",
        "12?channel_binding=SECRETxyz",
        "12?options=SECRETxyz",
        "12?application_name=SECRETxyz",
        "12?sslmode=SECRETxyz",
        "12/appdb?host=SECRETxyz",
        "12/appdb?password=SECRETxyz",
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
    fn unknown_database_url_parameters_are_named_when_they_look_like_names() {
        for key in ["host", "hostaddr", "port", "target_session_attrs"] {
            let url = format!("postgres://u@db.example.com/appdb?{key}=x");
            let reason = DatabaseUrl::parse(&url).unwrap_err();
            assert!(
                reason.starts_with(&format!("unsupported parameter `{key}`")),
                "{reason}"
            );
            assert!(!reason.contains("percent-encode"), "{reason}");
        }
        let reason =
            DatabaseUrl::parse("postgres://u@db.example.com/appdb?SECRETxyz=1").unwrap_err();
        assert!(
            reason.contains("unsupported parameter (name not shown)"),
            "{reason}"
        );
        assert!(!reason.contains("SECRETxyz"), "{reason}");
    }

    #[test]
    fn a_raw_at_sign_in_the_query_is_rejected() {
        for url in [
            "postgres://iron_oxide:12?host=SECRETxyz@127.0.0.1:5470/iron_oxide",
            "postgres://iron_oxide:?dbname=SECRETxyz@127.0.0.1:5470/iron_oxide",
            "postgres://iron_oxide:12?channel_binding=SECRETxyz@127.0.0.1:5470/iron_oxide",
            "postgres://iron_oxide:12/iron_oxide?host=SECRETxyz@127.0.0.1:5470/x",
        ] {
            let reason = DatabaseUrl::parse(url).unwrap_err();
            assert!(reason.contains("raw @"), "{url}: {reason}");
            assert!(!reason.contains("SECRETxyz"), "{url}: {reason}");
        }
        // Encoded, an `@` in a parameter value is fine.
        DatabaseUrl::parse("postgres://u@db.example.com/appdb?application_name=a%40b").unwrap();
    }

    #[test]
    fn neon_parameters_are_accepted_and_kept_away_from_sqlx() {
        let url = "postgresql://u:pw@ep-x.eu-central-1.aws.neon.tech/neondb?sslmode=require\
                   &channel_binding=require&connect_timeout=10&sslnegotiation=direct\
                   &options=endpoint%3Dep-x&application_name=iron-oxide";
        let parsed = DatabaseUrl::parse(url).unwrap();
        let kept = parsed.secret.expose_secret();
        for stripped in ["channel_binding", "connect_timeout", "sslnegotiation"] {
            assert!(!kept.contains(stripped), "{kept}");
        }
        for key in [
            "sslmode=require",
            "options=endpoint%3Dep-x",
            "application_name=iron-oxide",
        ] {
            assert!(kept.contains(key), "{kept}");
        }
        parsed.connect_options().unwrap();
        // Only stripped parameters: no dangling `?`.
        let parsed = DatabaseUrl::parse("postgres://u@h/db?channel_binding=require").unwrap();
        assert_eq!(parsed.secret.expose_secret(), "postgres://u@h/db");
    }

    #[test]
    fn a_unix_socket_host_is_shown_as_its_path() {
        let parsed = DatabaseUrl::parse("postgres://u@%2Ftmp/appdb").unwrap();
        assert_eq!(parsed.redacted(), "/tmp/appdb");
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
    fn stripe_webhook_secret_is_optional() {
        let config = load(&minimal()).unwrap();
        assert!(config.billing.stripe_webhook_secret.is_none());
        // Blank counts as unset, like every variable.
        let blank = load(&set(minimal(), vars::STRIPE_WEBHOOK_SECRET, "  ")).unwrap();
        assert!(blank.billing.stripe_webhook_secret.is_none());
    }

    #[test]
    fn stripe_webhook_secret_is_loaded_and_redacted() {
        let vars = set(
            minimal(),
            vars::STRIPE_WEBHOOK_SECRET,
            " test-placeholder-signing-secret ",
        );
        let config = load(&vars).unwrap();
        let secret = config.billing.stripe_webhook_secret.as_ref().unwrap();
        assert_eq!(secret.expose_secret(), "test-placeholder-signing-secret");
        let debug = format!("{config:?}");
        assert!(!debug.contains("placeholder-signing"), "{debug}");
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
