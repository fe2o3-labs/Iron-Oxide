//! Rate limits through the real router. The tests without a database use an unreachable pool
//! (nothing they send reaches Postgres); the others are `#[ignore = "needs Postgres"]`, like the
//! sign-in integration tests.

use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    time::Duration,
};

use dioxus::server::axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware::from_fn_with_state,
    response::Response,
    routing::{get, post},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use super::{
    ClientIpSource, GroupLimits, Limits, ROUTES, RateLimitConfig, RateLimiter, RouteGroup,
    client_ip::FLY_CLIENT_IP, limiter::Quota, per_ip,
};
use crate::server::{
    auth::test_support::{Browser, Passkey, TestApp},
    config::GOOGLE_CALLBACK_PATH,
    db,
};

const SIGN_IN_BEGIN: &str = "/api/auth/passkey/sign-in/begin";
const SIGN_UP_BEGIN: &str = "/api/auth/passkey/sign-up/begin";
const SIGN_UP_FINISH: &str = "/api/auth/passkey/sign-up/finish";
const GOOGLE_BEGIN: &str = "/api/auth/google/begin";
const REMOVE: &str = "/api/auth/passkey/remove";
const ME: &str = "/api/auth/me";

const HOUR: Duration = Duration::from_secs(3600);

fn quota(burst: u32, period: Duration) -> Quota {
    Quota::new(NonZeroU32::new(burst).unwrap(), period).unwrap()
}

/// Everything unlimited in practice, except what a test tightens.
fn generous() -> Limits {
    let wide = GroupLimits {
        per_ip: Some(quota(10_000, Duration::from_millis(1))),
        per_user: Some(quota(10_000, Duration::from_millis(1))),
    };
    Limits {
        auth_begin: wide,
        auth_finish: wide,
        google_callback: wide,
        session: wide,
        account: wide,
        write: wide,
        capacity: 1_000,
    }
}

fn config(client_ip: ClientIpSource, limits: Limits) -> RateLimitConfig {
    RateLimitConfig { client_ip, limits }
}

fn ip(raw: &str) -> IpAddr {
    raw.parse().unwrap()
}

// --- Middleware alone: a stub app behind the per-IP layer. ------------------------------------

/// A stub app with a begin route, a write route and the health route, behind [`per_ip`].
fn stub(limiter: RateLimiter) -> Router {
    Router::new()
        .route(SIGN_IN_BEGIN, post(|| async { "begun" }))
        .route("/api/sets", post(|| async { "saved" }))
        .route("/healthz", get(|| async { "ok" }))
        .layer(from_fn_with_state(limiter, per_ip))
}

async fn send(
    router: &Router,
    method: Method,
    path: &str,
    peer: Option<IpAddr>,
    headers: &[(&'static str, &str)],
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(peer) = peer {
        request = request.extension(ConnectInfo(SocketAddr::new(peer, 1234)));
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn begin(router: &Router, peer: &str, headers: &[(&'static str, &str)]) -> StatusCode {
    send(router, Method::POST, SIGN_IN_BEGIN, Some(ip(peer)), headers)
        .await
        .status()
}

fn retry_after(headers: &HeaderMap) -> u64 {
    headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn begin_limit(burst: u32, period: Duration) -> Limits {
    let mut limits = generous();
    limits.auth_begin.per_ip = Some(quota(burst, period));
    limits
}

#[tokio::test(start_paused = true)]
async fn a_burst_past_the_limit_gets_429_with_retry_after_until_it_refills() {
    let limiter = RateLimiter::new(&config(
        ClientIpSource::Peer,
        begin_limit(3, Duration::from_secs(20)),
    ));
    let app = stub(limiter);
    for _ in 0..3 {
        assert_eq!(begin(&app, "203.0.113.1", &[]).await, StatusCode::OK);
    }
    let response = send(
        &app,
        Method::POST,
        SIGN_IN_BEGIN,
        Some(ip("203.0.113.1")),
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(response.headers()), 20);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], 429);
    assert_eq!(body["data"]["retry_after_secs"], 20);

    tokio::time::advance(Duration::from_millis(5_500)).await;
    let response = send(
        &app,
        Method::POST,
        SIGN_IN_BEGIN,
        Some(ip("203.0.113.1")),
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(response.headers()), 15, "14.5 s, rounded up");

    tokio::time::advance(Duration::from_millis(14_500)).await;
    assert_eq!(begin(&app, "203.0.113.1", &[]).await, StatusCode::OK);
    assert_eq!(
        begin(&app, "203.0.113.1", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "one period earns one request back"
    );
}

#[tokio::test]
async fn limits_are_per_ip() {
    let app = stub(RateLimiter::new(&config(
        ClientIpSource::Peer,
        begin_limit(2, HOUR),
    )));
    for _ in 0..2 {
        assert_eq!(begin(&app, "203.0.113.1", &[]).await, StatusCode::OK);
    }
    assert_eq!(
        begin(&app, "203.0.113.1", &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(begin(&app, "203.0.113.2", &[]).await, StatusCode::OK);
    // An IPv6 client cannot escape by changing the low 64 bits.
    assert_eq!(begin(&app, "2001:db8::1", &[]).await, StatusCode::OK);
    assert_eq!(begin(&app, "2001:db8::2", &[]).await, StatusCode::OK);
    assert_eq!(
        begin(&app, "2001:db8::3", &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(begin(&app, "2001:db8:0:1::1", &[]).await, StatusCode::OK);
}

#[tokio::test]
async fn groups_have_independent_buckets() {
    let mut limits = begin_limit(1, HOUR);
    limits.write.per_ip = Some(quota(1, HOUR));
    let app = stub(RateLimiter::new(&config(ClientIpSource::Peer, limits)));
    let peer = Some(ip("203.0.113.1"));
    assert_eq!(begin(&app, "203.0.113.1", &[]).await, StatusCode::OK);
    assert_eq!(
        begin(&app, "203.0.113.1", &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let write = send(&app, Method::POST, "/api/sets", peer, &[]).await;
    assert_eq!(
        write.status(),
        StatusCode::OK,
        "the write bucket is separate"
    );
    let write = send(&app, Method::POST, "/api/sets", peer, &[]).await;
    assert_eq!(write.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_spoofed_fly_client_ip_is_ignored_in_peer_mode() {
    let app = stub(RateLimiter::new(&config(
        ClientIpSource::Peer,
        begin_limit(2, HOUR),
    )));
    // A new Fly-Client-IP on each request, from one peer (even a private one).
    for peer in ["203.0.113.1", "172.16.0.9"] {
        let mut statuses = Vec::new();
        for i in 0..4 {
            let spoofed = format!("198.51.100.{i}");
            statuses.push(begin(&app, peer, &[(FLY_CLIENT_IP, &spoofed)]).await);
        }
        assert_eq!(
            statuses,
            [
                StatusCode::OK,
                StatusCode::OK,
                StatusCode::TOO_MANY_REQUESTS,
                StatusCode::TOO_MANY_REQUESTS
            ],
            "{peer}"
        );
    }
}

#[tokio::test]
async fn fly_mode_trusts_the_header_only_from_the_proxy() {
    let app = stub(RateLimiter::new(&config(
        ClientIpSource::Fly,
        begin_limit(1, HOUR),
    )));
    // Through Fly's proxy: each client gets its own bucket.
    let proxy = "172.16.5.6";
    assert_eq!(
        begin(&app, proxy, &[(FLY_CLIENT_IP, "203.0.113.1")]).await,
        StatusCode::OK
    );
    assert_eq!(
        begin(&app, proxy, &[(FLY_CLIENT_IP, "203.0.113.1")]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        begin(&app, proxy, &[(FLY_CLIENT_IP, "203.0.113.2")]).await,
        StatusCode::OK
    );
    // A direct connection from the internet cannot pick its bucket with the header.
    let direct = "198.51.100.50";
    assert_eq!(
        begin(&app, direct, &[(FLY_CLIENT_IP, "192.0.2.1")]).await,
        StatusCode::OK
    );
    assert_eq!(
        begin(&app, direct, &[(FLY_CLIENT_IP, "192.0.2.2")]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // No usable header: the proxy's own address, one bucket for all such requests.
    assert_eq!(begin(&app, proxy, &[]).await, StatusCode::OK);
    assert_eq!(
        begin(&app, proxy, &[(FLY_CLIENT_IP, "garbage")]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn requests_without_connection_info_share_one_bucket() {
    let app = stub(RateLimiter::new(&config(
        ClientIpSource::Fly,
        begin_limit(1, HOUR),
    )));
    let first = send(
        &app,
        Method::POST,
        SIGN_IN_BEGIN,
        None,
        &[(FLY_CLIENT_IP, "192.0.2.1")],
    );
    assert_eq!(first.await.status(), StatusCode::OK);
    let second = send(
        &app,
        Method::POST,
        SIGN_IN_BEGIN,
        None,
        &[(FLY_CLIENT_IP, "192.0.2.2")],
    );
    assert_eq!(second.await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn memory_stays_bounded_under_many_client_ips() {
    let mut limits = begin_limit(1, HOUR);
    limits.capacity = 100;
    let limiter = RateLimiter::new(&config(ClientIpSource::Peer, limits));
    let app = stub(limiter.clone());
    for i in 0..5_000_u32 {
        let [_, _, a, b] = (0x0a00_0000 + i).to_be_bytes();
        let peer = IpAddr::from([198, 18, a, b]);
        let response = send(&app, Method::POST, SIGN_IN_BEGIN, Some(peer), &[]).await;
        assert_eq!(response.status(), StatusCode::OK, "a new client is served");
        assert!(limiter.ip_keys(RouteGroup::AuthBegin) <= 100);
    }
    // IPv6 clients each with their own /64.
    for i in 0..5_000_u128 {
        let peer = IpAddr::from((0x2001_0db8_u128 << 96 | i << 64).to_be_bytes());
        send(&app, Method::POST, SIGN_IN_BEGIN, Some(peer), &[]).await;
        assert!(limiter.ip_keys(RouteGroup::AuthBegin) <= 100);
    }
    // The client that is limited the most when the table fills up stays limited.
    let limiter = RateLimiter::new(&config(ClientIpSource::Peer, {
        let mut limits = begin_limit(2, HOUR);
        limits.capacity = 100;
        limits
    }));
    let app = stub(limiter.clone());
    for _ in 0..2 {
        assert_eq!(begin(&app, "203.0.113.1", &[]).await, StatusCode::OK);
    }
    for i in 0..200_u8 {
        assert_eq!(
            begin(&app, &format!("198.18.0.{i}"), &[]).await,
            StatusCode::OK
        );
        assert!(limiter.ip_keys(RouteGroup::AuthBegin) <= 100);
    }
    assert_eq!(
        begin(&app, "203.0.113.1", &[]).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn health_checks_and_page_loads_are_never_limited() {
    let mut limits = generous();
    for group in [
        &mut limits.auth_begin,
        &mut limits.auth_finish,
        &mut limits.google_callback,
        &mut limits.session,
        &mut limits.account,
        &mut limits.write,
    ] {
        group.per_ip = Some(quota(1, HOUR));
    }
    let app = stub(RateLimiter::new(&config(ClientIpSource::Peer, limits)));
    let peer = Some(ip("203.0.113.1"));
    for path in [SIGN_IN_BEGIN, "/api/sets"] {
        send(&app, Method::POST, path, peer, &[]).await;
        let limited = send(&app, Method::POST, path, peer, &[]).await;
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    for _ in 0..100 {
        let health = send(&app, Method::GET, "/healthz", peer, &[]).await;
        assert_eq!(health.status(), StatusCode::OK);
        let head = send(&app, Method::HEAD, "/healthz", peer, &[]).await;
        assert_eq!(head.status(), StatusCode::OK);
    }
}

// --- The real router. --------------------------------------------------------------------------

/// `POST path` with a JSON body, returning the raw response.
async fn post_raw(browser: &mut Browser, path: &str, body: Value) -> Response {
    let request = browser
        .request("POST", path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    browser.send(request).await
}

#[tokio::test]
async fn health_checks_are_never_limited_by_the_real_router() {
    let mut limits = generous();
    limits.write.per_ip = Some(quota(1, HOUR));
    limits.session.per_ip = Some(quota(1, HOUR));
    let app = TestApp::with_rate_limit(
        db::tests::unreachable_pool(),
        config(ClientIpSource::Peer, limits),
    )
    .await;
    let mut browser = app.browser();
    post_raw(&mut browser, ME, json!({})).await;
    let limited = post_raw(&mut browser, ME, json!({})).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key(header::RETRY_AFTER));
    for _ in 0..50 {
        let (status, _, body) = browser.get("/healthz").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
    }
    // /readyz is not limited either: it answers 503 (no database here), never 429.
    let (status, _, _) = browser.get("/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn every_route_in_the_table_exists() {
    let app = TestApp::with_rate_limit(
        db::tests::unreachable_pool(),
        config(ClientIpSource::Peer, generous()),
    )
    .await;
    let mut browser = app.browser();
    let unknown = post_raw(&mut browser, "/api/auth/no-such-route", json!({})).await;
    let not_a_route = unknown.status();
    assert!(not_a_route.is_client_error(), "{not_a_route}");
    for (path, _) in ROUTES {
        let status = post_raw(&mut browser, path, json!({})).await.status();
        assert_ne!(status, not_a_route, "{path} is not a route");
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }
    let (status, _, _) = browser.get(GOOGLE_CALLBACK_PATH).await;
    assert_ne!(status, StatusCode::NOT_FOUND);
}

async fn count(db: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_limited_begin_creates_no_session_and_no_ceremony(db: PgPool) {
    let mut limits = generous();
    limits.auth_begin.per_ip = Some(quota(3, HOUR));
    let app = TestApp::with_rate_limit(db.clone(), config(ClientIpSource::Peer, limits)).await;
    let mut statuses = Vec::new();
    for i in 0..50 {
        // A fresh cookie-less browser each time: the known exposure from #59's review.
        let mut browser = app.browser();
        let path = if i % 2 == 0 {
            SIGN_IN_BEGIN
        } else {
            GOOGLE_BEGIN
        };
        let body = if path == GOOGLE_BEGIN {
            json!({ "intent": "SignIn", "popup": false })
        } else {
            json!({})
        };
        let response = post_raw(&mut browser, path, body).await;
        statuses.push(response.status());
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            assert!(!response.headers().contains_key(header::SET_COOKIE));
        }
    }
    let ok = statuses.iter().filter(|s| s.is_success()).count();
    let limited = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((ok, limited), (3, 47), "{statuses:?}");
    assert_eq!(count(&db, "sessions").await, 3);
    assert_eq!(count(&db, "auth_ceremonies").await, 3);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_client_sees_a_429_server_error_with_the_delay(db: PgPool) {
    let mut limits = generous();
    limits.auth_begin.per_ip = Some(quota(1, Duration::from_secs(45)));
    let app = TestApp::with_rate_limit(db, config(ClientIpSource::Peer, limits)).await;
    let mut browser = app.browser();
    browser
        .call::<Value>(SIGN_UP_BEGIN, json!({ "display_name": "a" }))
        .await
        .unwrap();
    let response = post_raw(&mut browser, SIGN_UP_BEGIN, json!({ "display_name": "a" })).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(response.headers()), 45);
    // The body a server function's own error has: the client decodes it into
    // `ServerFnError::ServerError { message, code: 429, details: Some(data) }`.
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        json!({
            "message": "Too many requests. Please try again in 45 seconds.",
            "code": 429,
            "data": { "retry_after_secs": 45 },
        })
    );
}

/// Signs up a new account on `browser`.
async fn sign_up(browser: &mut Browser, name: &str) {
    let ccr: webauthn_rs_proto::CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": name }))
        .await
        .unwrap();
    let credential = Passkey::new().register(ccr);
    browser
        .call::<Value>(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap();
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn per_user_limits_are_independent_behind_one_ip(db: PgPool) {
    let mut limits = generous();
    limits.account.per_user = Some(quota(2, HOUR));
    let app = TestApp::with_rate_limit(db, config(ClientIpSource::Peer, limits)).await;
    // Two users behind the same IP (the default peer).
    let (mut alice, mut bob) = (app.browser(), app.browser());
    sign_up(&mut alice, "alice").await;
    sign_up(&mut bob, "bob").await;
    let unknown = json!({ "passkey_id": uuid::Uuid::now_v7() });
    for _ in 0..2 {
        let status = post_raw(&mut alice, REMOVE, unknown.clone()).await.status();
        assert_eq!(status, StatusCode::NOT_FOUND, "counted, then handled");
    }
    let limited = post_raw(&mut alice, REMOVE, unknown.clone()).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after(limited.headers()) > 0);
    // Bob, same IP, is not affected; nor is Alice's `me` (another group).
    assert_eq!(
        post_raw(&mut bob, REMOVE, unknown.clone()).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_raw(&mut alice, ME, json!({})).await.status(),
        StatusCode::OK
    );
    // Signed out, the per-user limit does not apply (the per-IP one still does).
    let mut carol = app.browser();
    for _ in 0..3 {
        let status = post_raw(&mut carol, REMOVE, unknown.clone()).await.status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_default_limits_let_a_normal_sign_up_and_sign_in_through(db: PgPool) {
    let app = TestApp::new(db).await;
    // A roomful of people behind one gym IP, each signing up once.
    for i in 0..20 {
        let mut browser = app.browser();
        sign_up(&mut browser, &format!("user {i}")).await;
    }
}

#[tokio::test]
async fn cross_site_requests_cannot_use_up_a_shared_ips_limits() {
    let mut limits = generous();
    limits.auth_begin.per_ip = Some(quota(3, HOUR));
    limits.google_callback.per_ip = Some(quota(3, HOUR));
    let app = TestApp::with_rate_limit(
        db::tests::unreachable_pool(),
        config(ClientIpSource::Peer, limits),
    )
    .await;
    // A page on another site, opened by someone behind the shared IP.
    let mut attacker = app.browser();
    attacker.origin = Some("https://evil.example".to_owned());
    attacker.fetch_site = Some("cross-site".to_owned());
    for _ in 0..50 {
        let status = post_raw(&mut attacker, SIGN_IN_BEGIN, json!({}))
            .await
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // It can load the Google callback as often as it likes (a cross-site GET, e.g. an <img>):
    // that uses up the callback's own bucket only.
    for _ in 0..10 {
        attacker.get(GOOGLE_CALLBACK_PATH).await;
    }
    let (status, headers, _) = attacker.get(GOOGLE_CALLBACK_PATH).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    // Someone else on the same IP still reaches the sign-in functions (the unreachable database
    // then fails them, but they are not rate limited).
    let mut neighbour = app.browser();
    assert_eq!(neighbour.peer, attacker.peer);
    let status = post_raw(&mut neighbour, SIGN_IN_BEGIN, json!({}))
        .await
        .status();
    assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(status, StatusCode::FORBIDDEN);
    let finish = "/api/auth/passkey/sign-in/finish";
    let status = post_raw(&mut neighbour, finish, json!({})).await.status();
    assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
}
