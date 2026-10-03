//! Endpoint test harness (#68): server functions called through the real router, as signed-in
//! users.
//!
//! [`TestApi::user`] signs a new user up with a software passkey (#5's test support), so every
//! call goes through the session cookie, the CSRF layer, `AuthUser` and the server function's
//! JSON encoding, exactly as from the browser. [`TestUser::id`] is the same user as the
//! repository's owner key, to seed their data with `server::db::testing`.
//!
//! Every endpoint that takes an id needs an isolation test named `another_users_*` (CI counts
//! them): user B gets exactly the `404` of an id that does not exist for A's ids, see
//! [`assert_not_found_for_other_user`].
//!
//! ```rust,ignore
//! #[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
//! #[ignore = "needs Postgres"]
//! async fn another_users_session_is_not_found(db: PgPool) {
//!     let api = TestApi::new(db.clone()).await;
//!     let (a, mut b) = api.users_a_and_b().await;
//!     let session = db_testing::session(&db, a.id).await;
//!     assert_not_found_for_other_user(&mut b, "/api/sessions/get", session.as_uuid(), |id| {
//!         json!({ "session_id": id })
//!     })
//!     .await;
//! }
//! ```

use dioxus::server::axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header, request},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{PgPool, types::Uuid};

use webauthn_rs_proto::RequestChallengeResponse;

use crate::api::error::ApiFailure;
use crate::auth::types::Me;
use crate::server::api::errors_layer::tests::client_error;
pub use crate::server::auth::test_support::CallError;
use crate::server::{
    api::error::{NOT_FOUND, UNAUTHORIZED},
    auth::test_support::{Browser, Passkey, TestApp, sign_up},
    db::ids::UserId,
};

/// The app under test: the full router over a fresh, migrated database.
pub struct TestApi {
    app: TestApp,
    pub db: PgPool,
}

impl TestApi {
    pub async fn new(db: PgPool) -> Self {
        Self {
            app: TestApp::new(db.clone()).await,
            db,
        }
    }

    /// A new user, signed up with a passkey and signed in, in their own browser.
    pub async fn user(&self, name: &str) -> TestUser {
        let mut browser = self.app.browser();
        let (me, _) = sign_up(&mut browser, &mut Passkey::new(), name).await;
        TestUser {
            id: me.user_id.into(),
            browser,
        }
    }

    /// A new user like [`TestApi::user`], with their passkey and its credential id, to sign in
    /// again with [`TestApi::sign_in`].
    pub async fn user_with_passkey(&self, name: &str) -> (TestUser, Passkey, Vec<u8>) {
        let mut browser = self.app.browser();
        let mut passkey = Passkey::new();
        let (me, credential_id) = sign_up(&mut browser, &mut passkey, name).await;
        let user = TestUser {
            id: me.user_id.into(),
            browser,
        };
        (user, passkey, credential_id)
    }

    /// Signs in with `passkey` in a new browser: a new session of the passkey's user.
    pub async fn sign_in(&self, passkey: &mut Passkey, credential_id: &[u8]) -> TestUser {
        let mut browser = self.app.browser();
        let rcr: RequestChallengeResponse = browser
            .call("/api/auth/passkey/sign-in/begin", serde_json::json!({}))
            .await
            .unwrap();
        let credential = passkey.sign_in(rcr, credential_id);
        let me: Me = browser
            .call(
                "/api/auth/passkey/sign-in/finish",
                serde_json::json!({ "credential": credential }),
            )
            .await
            .unwrap();
        TestUser {
            id: me.user_id.into(),
            browser,
        }
    }

    /// A, whose data the test creates, and B, who tries to reach it.
    pub async fn users_a_and_b(&self) -> (TestUser, TestUser) {
        (self.user("A").await, self.user("B").await)
    }

    /// A browser with no session.
    pub fn signed_out(&self) -> Browser {
        self.app.browser()
    }
}

/// A signed-in user and their browser. A clone shares the session: use clones to send concurrent
/// requests as the same user.
#[derive(Clone)]
pub struct TestUser {
    /// The user's id, as the repository's owner key.
    pub id: UserId,
    browser: Browser,
}

impl TestUser {
    /// Calls the server function at `path` (a `POST` with `body` as its JSON arguments) and
    /// decodes its result. A failure is decoded as the Dioxus client does and classified as the
    /// UI would show it: [`CallError::message`] is [`ApiFailure::classify`]'s message.
    ///
    /// [`ApiFailure::classify`]: crate::api::error::ApiFailure::classify
    pub async fn call<T: DeserializeOwned>(
        &mut self,
        path: &str,
        body: Value,
    ) -> Result<T, CallError> {
        let (status, bytes) = self.browser.post_json(path, body).await;
        if status.is_success() {
            return Ok(serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{path}: {e}: {}", String::from_utf8_lossy(&bytes))));
        }
        let failure = ApiFailure::classify(&client_error(status, &bytes).await);
        Err(CallError {
            status,
            message: failure.message,
        })
    }

    /// The raw status and body of a call, to compare two answers byte for byte.
    pub async fn call_raw(&mut self, path: &str, body: Value) -> (StatusCode, Vec<u8>) {
        self.browser.post_json(path, body).await
    }

    /// A `POST` to `path` from this user's browser (cookie, same-origin headers, JSON content
    /// type), for a test that needs to control the body or headers. Send it with
    /// [`TestUser::send`].
    pub fn post(&self, path: &str) -> request::Builder {
        self.browser
            .request("POST", path)
            .header(header::CONTENT_TYPE, "application/json")
    }

    /// Sends `request` and returns the status and the body, as JSON when it is JSON (as a JSON
    /// string otherwise).
    pub async fn send(&mut self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.browser.send(request).await;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, body)
    }

    /// The same session in a browser on another site: its requests are cross-site.
    pub fn cross_site(&self) -> Self {
        let mut other = self.clone();
        other.browser.origin = Some("https://evil.example".to_owned());
        other.browser.fetch_site = Some("cross-site".to_owned());
        other
    }

    /// Whether this browser still holds a session cookie.
    pub fn has_cookie(&self) -> bool {
        self.browser.cookie.is_some()
    }

    /// Like [`TestUser::call`], for a call that must fail. Panics with the result if it succeeds.
    pub async fn call_err(&mut self, path: &str, body: Value) -> CallError {
        match self.call::<Value>(path, body).await {
            Ok(value) => panic!("{path} succeeded: {value}"),
            Err(error) => error,
        }
    }
}

/// Asserts that `other` gets `404 Not found.` when calling `path` with the owner's id
/// (`body(owners_id)`), exactly as for an id that exists for nobody (`body(random id)`), so the
/// response tells nothing about the owner's data.
///
/// It only checks what `other` sees. The test must also show that the owner's call succeeds (or
/// that the id is really the owner's) and, for writes, that the owner's data did not change.
pub async fn assert_not_found_for_other_user(
    other: &mut TestUser,
    path: &str,
    owners_id: Uuid,
    body: impl Fn(Uuid) -> Value,
) {
    let with_owners_id = other.call_err(path, body(owners_id)).await;
    let with_random_id = other.call_err(path, body(Uuid::now_v7())).await;
    // The same bytes on the wire, not only the same message.
    assert_eq!(
        other.call_raw(path, body(owners_id)).await,
        other.call_raw(path, body(Uuid::now_v7())).await,
        "{path}: the answers differ"
    );
    let not_found = CallError {
        status: StatusCode::NOT_FOUND,
        message: NOT_FOUND.to_owned(),
    };
    assert_eq!(with_owners_id, not_found, "{path} with the owner's id");
    assert_eq!(with_random_id, not_found, "{path} with a random id");
}

/// Asserts that calling `path` without a session is `401 Please sign in.`.
pub async fn assert_unauthorized_when_signed_out(api: &TestApi, path: &str, body: Value) {
    let error = api
        .signed_out()
        .call::<Value>(path, body)
        .await
        .expect_err("a signed-out call must fail");
    assert_eq!(
        error,
        CallError {
            status: StatusCode::UNAUTHORIZED,
            message: UNAUTHORIZED.to_owned(),
        },
        "{path}"
    );
}
