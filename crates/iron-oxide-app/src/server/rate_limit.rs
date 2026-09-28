//! Rate limiting (#23): per-IP limits on every state-changing request and the sign-in routes,
//! and per-user limits on authenticated writes. Design, limits and contract:
//! `docs/rate-limiting.md`.
//!
//! Two middlewares share one [`RateLimiter`]:
//! - [`per_ip`] runs just inside the CSRF check and before the session is loaded or any handler
//!   runs, so a refused request touches neither the database nor the session table. Being inside
//!   the CSRF check matters: a cross-site page opened by someone behind a shared IP must not be
//!   able to use up that IP's sign-in limits with requests the CSRF check refuses anyway;
//! - [`per_user`] runs inside the session layer and keys signed-in requests by their user id.
//!
//! Each request belongs to at most one [`RouteGroup`] ([`classify`]); each group has its own
//! buckets and its own limits ([`Limits`]). Safe methods (`GET`, `HEAD`, ...) are never limited,
//! except the Google callback, so page loads, assets and the health checks never are.
//!
//! # Giving a new write endpoint its own limits
//!
//! Every `POST` (and other unsafe method) already gets the default [`RouteGroup::Write`] limits:
//! per IP, and per user when signed in. For an endpoint that needs stricter ones (an import, an
//! upload, deleting the account):
//! 1. add a variant to [`RouteGroup`] (and to [`RouteGroup::ALL`]);
//! 2. give it a [`GroupLimits`] in [`Limits`] and its `Default`;
//! 3. map the endpoint's path to it in [`ROUTES`];
//! 4. add a test in `integration_tests.rs` that bursts past the new limit.
//!
//! The `every_route_in_the_table_exists` test fails if a path in [`ROUTES`] is not a real route.

pub mod client_ip;
pub mod limiter;

#[cfg(test)]
mod integration_tests;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use dioxus::logger::tracing;
use dioxus::server::axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::time::Instant;
use tower_sessions::Session;

pub use self::client_ip::ClientIpSource;
use self::{
    client_ip::{ClientKey, client_ip},
    limiter::{DEFAULT_CAPACITY, KeyedLimiter, Limited, Quota, retry_after_secs},
};
use super::{auth::session::keys, config::GOOGLE_CALLBACK_PATH};
use crate::{auth::types::UserId, rate_limit::RETRY_AFTER_SECS};

const MINUTE: Duration = Duration::from_secs(60);

/// A family of routes sharing limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteGroup {
    /// Starting a ceremony: each call can create a session row and a ceremony row.
    AuthBegin,
    /// Finishing a passkey ceremony (WebAuthn verification).
    AuthFinish,
    /// The Google callback: a `GET` that any page can trigger (e.g. with an `<img>`), so it has
    /// its own bucket and cannot use up the passkey finishes'.
    GoogleCallback,
    /// `me` (polled by the UI while waiting for Google) and sign-out: read-only or harmless.
    Session,
    /// Removing a passkey or unlinking Google.
    Account,
    /// Every other unsafe request (server functions that write).
    Write,
}

impl RouteGroup {
    pub const ALL: [Self; 6] = [
        Self::AuthBegin,
        Self::AuthFinish,
        Self::GoogleCallback,
        Self::Session,
        Self::Account,
        Self::Write,
    ];

    const fn index(self) -> usize {
        match self {
            Self::AuthBegin => 0,
            Self::AuthFinish => 1,
            Self::GoogleCallback => 2,
            Self::Session => 3,
            Self::Account => 4,
            Self::Write => 5,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::AuthBegin => "auth_begin",
            Self::AuthFinish => "auth_finish",
            Self::GoogleCallback => "google_callback",
            Self::Session => "session",
            Self::Account => "account",
            Self::Write => "write",
        }
    }
}

/// Unsafe-method routes with their own group. Anything else unsafe is [`RouteGroup::Write`].
pub const ROUTES: &[(&str, RouteGroup)] = &[
    ("/api/auth/passkey/sign-up/begin", RouteGroup::AuthBegin),
    ("/api/auth/passkey/sign-in/begin", RouteGroup::AuthBegin),
    ("/api/auth/passkey/add/begin", RouteGroup::AuthBegin),
    ("/api/auth/google/begin", RouteGroup::AuthBegin),
    ("/api/auth/passkey/sign-up/finish", RouteGroup::AuthFinish),
    ("/api/auth/passkey/sign-in/finish", RouteGroup::AuthFinish),
    ("/api/auth/passkey/add/finish", RouteGroup::AuthFinish),
    ("/api/auth/me", RouteGroup::Session),
    ("/api/auth/sign-out", RouteGroup::Session),
    ("/api/auth/passkey/remove", RouteGroup::Account),
    ("/api/auth/google/unlink", RouteGroup::Account),
];

/// The group of a request, or `None` when it is not limited.
#[must_use]
pub fn classify(method: &Method, path: &str) -> Option<RouteGroup> {
    // A cross-site GET by design, finishing a ceremony (axum also answers HEAD with it).
    if path == GOOGLE_CALLBACK_PATH && matches!(*method, Method::GET | Method::HEAD) {
        return Some(RouteGroup::GoogleCallback);
    }
    if matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    ) {
        return None;
    }
    Some(
        ROUTES
            .iter()
            .find(|(route, _)| *route == path)
            .map_or(RouteGroup::Write, |(_, group)| *group),
    )
}

/// One group's limits: per client IP, and per signed-in user. `None` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupLimits {
    pub per_ip: Option<Quota>,
    pub per_user: Option<Quota>,
}

/// Every group's limits, and how many keys each limiter remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub auth_begin: GroupLimits,
    pub auth_finish: GroupLimits,
    pub google_callback: GroupLimits,
    pub session: GroupLimits,
    pub account: GroupLimits,
    pub write: GroupLimits,
    /// The most keys (IPs or users) one limiter remembers; see [`limiter`].
    pub capacity: usize,
}

impl Default for Limits {
    /// The production limits. Rationale in `docs/rate-limiting.md`.
    fn default() -> Self {
        // Every quota is a constant, so an invalid one fails the build (see `Quota::per`).
        //
        // A gym's Wi-Fi or a carrier NAT puts a whole room behind one IP: a class signing in at
        // once must fit. One sign-in takes one or two begins.
        const SIGN_IN_PER_IP: Quota = Quota::per(30, MINUTE);
        // Adding a passkey, linking or unlinking Google: a handful per session at most.
        const ACCOUNT_PER_USER: Quota = Quota::per(10, Duration::from_secs(10 * 60));
        // The UI polls `me` every 2 s during a Google sign-in, for every user behind the IP:
        // 5 per second on average.
        const SESSION_PER_IP: Quota = Quota::per(300, MINUTE);
        const ACCOUNT_PER_IP: Quota = Quota::per(60, MINUTE);
        // The offline queue flushes a whole workout at once, for every user behind the IP:
        // 10 per second on average, 600 at once.
        const WRITE_PER_IP: Quota = Quota::per(600, MINUTE);
        // One user: a long workout's sets in one burst, then 2 per second.
        const WRITE_PER_USER: Quota = Quota::per(120, MINUTE);
        Self {
            auth_begin: GroupLimits {
                per_ip: Some(SIGN_IN_PER_IP),
                per_user: Some(ACCOUNT_PER_USER),
            },
            auth_finish: GroupLimits {
                per_ip: Some(SIGN_IN_PER_IP),
                per_user: Some(ACCOUNT_PER_USER),
            },
            // Per IP only: the per-user layer does not wrap this route (see `server::router`).
            google_callback: GroupLimits {
                per_ip: Some(SIGN_IN_PER_IP),
                per_user: None,
            },
            session: GroupLimits {
                per_ip: Some(SESSION_PER_IP),
                per_user: None,
            },
            account: GroupLimits {
                per_ip: Some(ACCOUNT_PER_IP),
                per_user: Some(ACCOUNT_PER_USER),
            },
            write: GroupLimits {
                per_ip: Some(WRITE_PER_IP),
                per_user: Some(WRITE_PER_USER),
            },
            capacity: DEFAULT_CAPACITY,
        }
    }
}

impl Limits {
    #[must_use]
    pub fn group(&self, group: RouteGroup) -> GroupLimits {
        match group {
            RouteGroup::AuthBegin => self.auth_begin,
            RouteGroup::AuthFinish => self.auth_finish,
            RouteGroup::GoogleCallback => self.google_callback,
            RouteGroup::Session => self.session,
            RouteGroup::Account => self.account,
            RouteGroup::Write => self.write,
        }
    }
}

/// The rate-limit settings: where client IPs come from (`CLIENT_IP_SOURCE`) and the limits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RateLimitConfig {
    pub client_ip: ClientIpSource,
    pub limits: Limits,
}

/// The limiters of every group, in memory on this machine. Cheap to clone.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    client_ip: ClientIpSource,
    groups: [GroupLimiters; RouteGroup::ALL.len()],
}

#[derive(Debug)]
struct GroupLimiters {
    per_ip: Option<KeyedLimiter<ClientKey>>,
    per_user: Option<KeyedLimiter<UserId>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(config: &RateLimitConfig) -> Self {
        let limits = &config.limits;
        let groups = RouteGroup::ALL.map(|group| {
            let quotas = limits.group(group);
            GroupLimiters {
                per_ip: quotas
                    .per_ip
                    .map(|quota| KeyedLimiter::new(quota, limits.capacity)),
                per_user: quotas
                    .per_user
                    .map(|quota| KeyedLimiter::new(quota, limits.capacity)),
            }
        });
        Self {
            inner: Arc::new(Inner {
                client_ip: config.client_ip,
                groups,
            }),
        }
    }

    fn group(&self, group: RouteGroup) -> &GroupLimiters {
        // `index` covers every variant and `groups` has one entry per variant.
        &self.inner.groups[group.index()]
    }

    /// How many keys the group's per-IP limiter remembers (for tests and diagnostics).
    #[cfg(test)]
    fn ip_keys(&self, group: RouteGroup) -> usize {
        self.group(group)
            .per_ip
            .as_ref()
            .map_or(0, KeyedLimiter::len)
    }
}

/// The per-IP middleware. Install it just inside the CSRF check, outside the session layer (see
/// `auth::install`).
pub async fn per_ip(State(limiter): State<RateLimiter>, request: Request, next: Next) -> Response {
    let Some(group) = classify(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let Some(ip_limiter) = &limiter.group(group).per_ip else {
        return next.run(request).await;
    };
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let key = ClientKey::of(client_ip(limiter.inner.client_ip, peer, request.headers()));
    match ip_limiter.check(key, Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(limited) => too_many_requests(group, "ip", request.uri().path(), limited),
    }
}

/// The per-user middleware. Install it inside the session layer (see `server::router`).
///
/// It only reads the user id from the session. It does not check the session's timeouts: that
/// is `AuthUser`'s job, and an expired session still counts against its user.
pub async fn per_user(
    State(limiter): State<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let Some(group) = classify(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let Some(user_limiter) = &limiter.group(group).per_user else {
        return next.run(request).await;
    };
    let Some(session) = request.extensions().get::<Session>().cloned() else {
        tracing::error!("rate limit: no session on the request, check server::router");
        return next.run(request).await;
    };
    let user = match session.get::<UserId>(keys::USER_ID).await {
        Ok(user) => user,
        Err(error) => {
            // The handler loads the same session and reports the failure.
            tracing::debug!(%error, "rate limit: cannot read the session");
            None
        }
    };
    let Some(user) = user else {
        return next.run(request).await;
    };
    match user_limiter.check(user, Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(limited) => too_many_requests(group, "user", request.uri().path(), limited),
    }
}

/// `429 Too Many Requests` with `Retry-After`. Server functions (`/api/`) get the server-function
/// error body, which the client decodes into `ServerFnError::ServerError { code: 429, details }`;
/// other routes (the Google callback page) get plain text.
fn too_many_requests(
    group: RouteGroup,
    by: &'static str,
    path: &str,
    limited: Limited,
) -> Response {
    let secs = retry_after_secs(limited.retry_after);
    tracing::debug!(
        group = group.name(),
        by,
        path,
        retry_after_secs = secs,
        "rate limited"
    );
    let message = format!(
        "Too many requests. Please try again in {}.",
        wait_text(secs)
    );
    let (content_type, body) = if path.starts_with("/api/") {
        let body = serde_json::json!({
            "message": message,
            "code": StatusCode::TOO_MANY_REQUESTS.as_u16(),
            "data": { RETRY_AFTER_SECS: secs },
        });
        ("application/json", body.to_string())
    } else {
        ("text/plain; charset=utf-8", message)
    };
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Body::from(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::RETRY_AFTER, HeaderValue::from(secs));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// "1 second", "42 seconds", "3 minutes" (rounded up).
fn wait_text(secs: u64) -> String {
    match secs {
        0 | 1 => "1 second".to_owned(),
        2..=59 => format!("{secs} seconds"),
        _ => match secs.div_ceil(60) {
            1 => "1 minute".to_owned(),
            minutes => format!("{minutes} minutes"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::body::to_bytes;

    #[test]
    fn the_sign_in_routes_have_their_own_groups() {
        let post = Method::POST;
        for (path, group) in ROUTES {
            assert_eq!(classify(&post, path), Some(*group), "{path}");
        }
        assert_eq!(
            classify(&post, "/api/auth/passkey/sign-in/begin"),
            Some(RouteGroup::AuthBegin)
        );
        assert_eq!(
            classify(&Method::GET, GOOGLE_CALLBACK_PATH),
            Some(RouteGroup::GoogleCallback)
        );
        assert_eq!(
            classify(&Method::HEAD, GOOGLE_CALLBACK_PATH),
            Some(RouteGroup::GoogleCallback)
        );
    }

    #[test]
    fn every_other_unsafe_request_is_a_write() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            for path in [
                "/api/sets",
                "/api/auth/passkey/sign-in/begin/",
                "/API/auth/me",
                "//api/auth/passkey/sign-in/begin",
                "/",
                GOOGLE_CALLBACK_PATH,
            ] {
                assert_eq!(
                    classify(&method, path),
                    Some(RouteGroup::Write),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn safe_requests_are_never_limited() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            for path in [
                "/healthz",
                "/readyz",
                "/",
                "/api/server-time",
                "/api/auth/me",
            ] {
                assert_eq!(classify(&method, path), None, "{method} {path}");
            }
        }
        assert_eq!(classify(&Method::OPTIONS, GOOGLE_CALLBACK_PATH), None);
    }

    #[test]
    fn every_group_has_limits_by_default() {
        let limits = Limits::default();
        for group in RouteGroup::ALL {
            assert!(limits.group(group).per_ip.is_some(), "{group:?}");
            assert_eq!(RouteGroup::ALL[group.index()], group);
        }
        assert!(limits.write.per_user.is_some());
        assert!(limits.auth_begin.per_user.is_some());
        assert!(limits.session.per_user.is_none());
        assert!(limits.google_callback.per_user.is_none());
        assert_eq!(limits.capacity, DEFAULT_CAPACITY);
    }

    #[test]
    fn wait_text_reads_naturally() {
        assert_eq!(wait_text(0), "1 second");
        assert_eq!(wait_text(1), "1 second");
        assert_eq!(wait_text(2), "2 seconds");
        assert_eq!(wait_text(59), "59 seconds");
        assert_eq!(wait_text(60), "1 minute");
        assert_eq!(wait_text(61), "2 minutes");
        assert_eq!(wait_text(600), "10 minutes");
    }

    async fn body(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_server_function_gets_the_server_function_error_body() {
        let limited = Limited {
            retry_after: Duration::from_millis(4_200),
        };
        let response = too_many_requests(
            RouteGroup::AuthBegin,
            "ip",
            "/api/auth/google/begin",
            limited,
        );
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "5");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let json: serde_json::Value = serde_json::from_str(&body(response).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "message": "Too many requests. Please try again in 5 seconds.",
                "code": 429,
                "data": { "retry_after_secs": 5 },
            })
        );
    }

    #[tokio::test]
    async fn the_callback_page_gets_plain_text() {
        let limited = Limited {
            retry_after: Duration::from_secs(90),
        };
        let response = too_many_requests(
            RouteGroup::GoogleCallback,
            "ip",
            GOOGLE_CALLBACK_PATH,
            limited,
        );
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "90");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        assert_eq!(
            body(response).await,
            "Too many requests. Please try again in 2 minutes."
        );
    }
}
