//! Billing seam (#21): the Stripe webhook endpoint, not implemented yet.
//!
//! `POST /webhooks/stripe` answers `501 Not Implemented` after reading (at most
//! [`STRIPE_WEBHOOK_BODY_LIMIT`] bytes of) the body. It is mounted outside the session and CSRF
//! layers (see `server::router`): Stripe's requests are cross-site `POST`s with no `Origin` and no
//! cookie, so the CSRF check would refuse them, and a session is meaningless for them. They are
//! authenticated by their signature instead, once implemented:
//!
//! TODO(billing): implement the handler as designed in `docs/billing.md`:
//! 1. Refuse unless `STRIPE_WEBHOOK_SECRET` is configured (the config variable already exists).
//! 2. Parse `Stripe-Signature` (`t=<unix seconds>,v1=<hex>[,v1=<hex>…][,v0=…]`); ignore every
//!    scheme but `v1` (downgrade protection). Several `v1` values appear while a secret is rolled.
//! 3. Compute HMAC-SHA256 over `"{t}.{raw body}"` with the endpoint secret and compare it with
//!    each `v1` in constant time. Always the raw bytes: never parse the JSON before verifying.
//! 4. Refuse a `t` more than 5 minutes away from now (Stripe's default tolerance): the replay
//!    window. Answer `400` for a bad signature or timestamp.
//! 5. Deduplicate by event id (`evt_…`): insert it into a `stripe_events` table first, in the
//!    same transaction as the plan change; an id already there is answered `200` with no effect.
//! 6. Apply the event (`users.plan`), answer `2xx` quickly.
//!
//! Until then, do not register this endpoint in Stripe: Stripe retries failed deliveries and
//! eventually disables an endpoint that keeps failing.

use dioxus::logger::tracing;
use dioxus::server::axum::{
    Router,
    body::Bytes,
    extract::DefaultBodyLimit,
    http::{HeaderMap, StatusCode},
    routing::post,
};

/// Where Stripe delivers events.
pub const STRIPE_WEBHOOK_PATH: &str = "/webhooks/stripe";

/// The largest webhook body accepted; bigger requests get `413`. Stripe events are a few KB (lists
/// inside an event, such as invoice lines, are truncated), so this is generous while bounding what
/// an unauthenticated caller can make the server buffer.
pub const STRIPE_WEBHOOK_BODY_LIMIT: usize = 256 * 1024;

/// The signature header Stripe sends with every event.
const STRIPE_SIGNATURE: &str = "stripe-signature";

/// The billing routes. Merge them outside the session and CSRF layers.
pub fn routes() -> Router {
    Router::new().route(
        STRIPE_WEBHOOK_PATH,
        post(stripe_webhook).layer(DefaultBodyLimit::max(STRIPE_WEBHOOK_BODY_LIMIT)),
    )
}

/// The Stripe webhook stub: reads the body (enforcing the size limit), then `501`.
///
/// The body is taken as raw bytes on purpose: signature verification needs them unchanged.
async fn stripe_webhook(headers: HeaderMap, body: Bytes) -> (StatusCode, &'static str) {
    // Nothing from the request is logged beyond its shape: it is unauthenticated input.
    tracing::debug!(
        signed = headers.contains_key(STRIPE_SIGNATURE),
        bytes = body.len(),
        "Stripe webhook received, but billing is not implemented yet"
    );
    (
        StatusCode::NOT_IMPLEMENTED,
        "Stripe webhooks are not handled yet.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{auth::test_support::TestApp, db};
    use dioxus::server::axum::{
        body::{Body, to_bytes},
        http::{Request, header},
        response::Response,
    };
    use tower::ServiceExt;

    /// A Stripe-like delivery: cross-site, no `Origin`, no cookie, a signature header.
    fn delivery(path: &str, body: Vec<u8>) -> Request<Body> {
        Request::post(path)
            .header(header::HOST, "localhost:8080")
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .header(
                header::USER_AGENT,
                "Stripe/1.0 (+https://stripe.com/docs/webhooks)",
            )
            .header(STRIPE_SIGNATURE, "t=1790000000,v1=00,v0=00")
            .body(Body::from(body))
            .unwrap()
    }

    async fn status_and_body(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn full_router() -> Router {
        TestApp::new(db::tests::unreachable_pool()).await.router
    }

    #[tokio::test]
    async fn the_stub_answers_501() {
        let response = routes()
            .oneshot(delivery(STRIPE_WEBHOOK_PATH, br#"{"id":"evt_1"}"#.to_vec()))
            .await
            .unwrap();
        let (status, body) = status_and_body(response).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body, "Stripe webhooks are not handled yet.");
    }

    #[tokio::test]
    async fn only_post_is_routed() {
        let request = Request::get(STRIPE_WEBHOOK_PATH)
            .body(Body::empty())
            .unwrap();
        let response = routes().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn the_body_limit_is_enforced() {
        let at_limit = delivery(STRIPE_WEBHOOK_PATH, vec![b' '; STRIPE_WEBHOOK_BODY_LIMIT]);
        let response = routes().oneshot(at_limit).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        let over = delivery(
            STRIPE_WEBHOOK_PATH,
            vec![b' '; STRIPE_WEBHOOK_BODY_LIMIT + 1],
        );
        let response = routes().oneshot(over).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn stripe_deliveries_are_not_blocked_by_the_csrf_check() {
        let router = full_router().await;
        // What Stripe sends: neither `Origin` nor `Sec-Fetch-Site`, which the CSRF layer refuses.
        let response = router
            .clone()
            .oneshot(delivery(STRIPE_WEBHOOK_PATH, br#"{"id":"evt_1"}"#.to_vec()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "no session for webhooks"
        );

        // Even an explicitly cross-site POST reaches the stub (the signature will be the guard).
        let mut cross_site = delivery(STRIPE_WEBHOOK_PATH, b"{}".to_vec());
        let headers = cross_site.headers_mut();
        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        let response = router.clone().oneshot(cross_site).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        // And the body limit holds in the full router.
        let over = delivery(
            STRIPE_WEBHOOK_PATH,
            vec![b' '; STRIPE_WEBHOOK_BODY_LIMIT + 1],
        );
        let response = router.clone().oneshot(over).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_exemption_is_only_the_webhook_route() {
        let router = full_router().await;
        // The same delivery aimed at a server function is refused by the CSRF layer.
        for path in [
            "/api/auth/sign-out",
            "/webhooks/stripe/",
            "/webhooks/stripe/extra",
            "/webhooks/other",
        ] {
            let response = router
                .clone()
                .oneshot(delivery(path, b"{}".to_vec()))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }
}
