//! CSRF protection for state-changing requests.
//!
//! The session cookie is `SameSite=Lax`, so browsers already leave it off cross-site `POST`s.
//! On top of that, this layer refuses every request with an unsafe method (anything but `GET`,
//! `HEAD`, `OPTIONS` and `TRACE`, so every server function that changes state) unless the browser
//! says it comes from our own origin:
//! - `Sec-Fetch-Site`, when present, must be `same-origin` (`same-site` is refused too: a sibling
//!   subdomain is not trusted);
//! - `Origin`, when present, must be exactly `APP_BASE_URL`'s origin (never compared with `Host`,
//!   which a proxy such as `dx serve` rewrites);
//! - at least one of the two must be present. Every browser that runs the app sends both on a
//!   `fetch` POST; a request with neither is not from our pages.
//!
//! `GET` endpoints must therefore never change state. The Google callback is a cross-site `GET`
//! by design and is protected by its one-time `state` instead.

use std::sync::Arc;

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;
use dioxus::server::axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// The header name, lowercase (not in `http::header` in the version axum uses).
const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// The one origin state-changing requests may come from.
#[derive(Debug, Clone)]
pub struct CsrfPolicy {
    /// ASCII serialization, e.g. `https://iron-oxyde.com` or `http://localhost:8080`.
    origin: Arc<str>,
}

impl CsrfPolicy {
    #[must_use]
    pub fn new(origin: &url::Url) -> Self {
        Self {
            origin: origin.origin().ascii_serialization().into(),
        }
    }

    /// The allowed origin, e.g. `https://iron-oxyde.com`.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Checks one request. `Err` carries the reason, for the log.
    pub fn check(&self, method: &Method, headers: &HeaderMap) -> Result<(), &'static str> {
        if is_safe(method) {
            return Ok(());
        }
        let fetch_site = headers.get(SEC_FETCH_SITE).map(|v| v.to_str());
        let origin = headers.get(header::ORIGIN).map(|v| v.to_str());
        match fetch_site {
            None => {}
            Some(Ok("same-origin")) => {}
            Some(Ok(_)) => return Err("Sec-Fetch-Site is not same-origin"),
            Some(Err(_)) => return Err("Sec-Fetch-Site is not ASCII"),
        }
        match origin {
            None => {}
            Some(Ok(origin)) if origin == &*self.origin => {}
            Some(_) => return Err("Origin is not the app origin"),
        }
        if fetch_site.is_none() && origin.is_none() {
            return Err("neither Sec-Fetch-Site nor Origin is present");
        }
        Ok(())
    }
}

/// Methods that must not change state (RFC 9110 "safe" methods).
fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// The middleware: `403` with a server-function error body for a refused request.
pub async fn guard(State(policy): State<CsrfPolicy>, request: Request, next: Next) -> Response {
    match policy.check(request.method(), request.headers()) {
        Ok(()) => next.run(request).await,
        Err(reason) => {
            tracing::warn!(
                reason,
                method = %request.method(),
                path = request.uri().path(),
                "cross-site request refused"
            );
            ServerFnError::ServerError {
                message: "Cross-site request refused.".to_owned(),
                code: 403,
                details: None,
            }
            .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::server::axum::http::HeaderValue;

    fn policy() -> CsrfPolicy {
        CsrfPolicy::new(&url::Url::parse("https://iron-oxyde.com").unwrap())
    }

    fn headers(pairs: &[(&'static str, &'static [u8])]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_bytes(value).unwrap());
        }
        map
    }

    #[test]
    fn origin_is_the_ascii_serialization() {
        assert_eq!(policy().origin(), "https://iron-oxyde.com");
        let local = CsrfPolicy::new(&url::Url::parse("http://localhost:8080/").unwrap());
        assert_eq!(local.origin(), "http://localhost:8080");
    }

    #[test]
    fn safe_methods_are_never_checked() {
        let cross = headers(&[
            ("sec-fetch-site", b"cross-site"),
            ("origin", b"https://evil.example"),
        ]);
        for method in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            assert_eq!(policy().check(&method, &cross), Ok(()), "{method}");
        }
    }

    #[test]
    fn same_origin_post_is_allowed() {
        let p = policy();
        let both = headers(&[
            ("sec-fetch-site", b"same-origin"),
            ("origin", b"https://iron-oxyde.com"),
        ]);
        assert_eq!(p.check(&Method::POST, &both), Ok(()));
        // Older browsers: only one of the two headers.
        let origin_only = headers(&[("origin", b"https://iron-oxyde.com")]);
        assert_eq!(p.check(&Method::POST, &origin_only), Ok(()));
        let fetch_only = headers(&[("sec-fetch-site", b"same-origin")]);
        assert_eq!(p.check(&Method::POST, &fetch_only), Ok(()));
    }

    #[test]
    fn every_unsafe_method_is_checked() {
        let cross = headers(&[("origin", b"https://evil.example")]);
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(policy().check(&method, &cross).is_err(), "{method}");
        }
        let custom = Method::from_bytes(b"PURGE").unwrap();
        assert!(policy().check(&custom, &cross).is_err());
    }

    #[test]
    fn cross_site_and_same_site_are_refused() {
        let p = policy();
        for site in [
            &b"cross-site"[..],
            b"same-site",
            b"none",
            b"SAME-ORIGIN",
            b"",
        ] {
            let h = headers(&[("sec-fetch-site", site)]);
            assert!(p.check(&Method::POST, &h).is_err(), "{site:?}");
        }
    }

    #[test]
    fn a_foreign_origin_is_refused_even_with_same_origin_fetch_site() {
        let p = policy();
        for origin in [
            &b"https://evil.example"[..],
            b"https://app.iron-oxyde.com",
            b"http://iron-oxyde.com",
            b"https://iron-oxyde.com:443",
            b"https://iron-oxyde.com/",
            b"https://iron-oxyde.com.evil.example",
            b"null",
            b"",
            b"https://iron-oxyd\xc3\xa9.com",
        ] {
            let h = headers(&[("sec-fetch-site", b"same-origin"), ("origin", origin)]);
            assert!(p.check(&Method::POST, &h).is_err(), "{origin:?}");
        }
    }

    #[test]
    fn a_post_without_origin_information_is_refused() {
        assert_eq!(
            policy().check(&Method::POST, &HeaderMap::new()),
            Err("neither Sec-Fetch-Site nor Origin is present")
        );
    }

    #[test]
    fn non_ascii_fetch_site_is_refused() {
        let h = headers(&[("sec-fetch-site", b"same-origin\xff")]);
        assert!(policy().check(&Method::POST, &h).is_err());
    }
}
