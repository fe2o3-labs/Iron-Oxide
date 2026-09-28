//! Sign-in integration tests: the real router and server functions against Postgres
//! (`#[ignore = "needs Postgres"]`, run by the CI integration job), a software passkey and a
//! local mock of Google. Each `sqlx::test` gets a fresh, migrated database.

use dioxus::server::axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

use super::test_support::{Browser, Grant, Passkey, TestApp, query_param};
use crate::auth::types::Me;

const SIGN_UP_BEGIN: &str = "/api/auth/passkey/sign-up/begin";
const SIGN_UP_FINISH: &str = "/api/auth/passkey/sign-up/finish";
const SIGN_IN_BEGIN: &str = "/api/auth/passkey/sign-in/begin";
const SIGN_IN_FINISH: &str = "/api/auth/passkey/sign-in/finish";
const ADD_BEGIN: &str = "/api/auth/passkey/add/begin";
const ADD_FINISH: &str = "/api/auth/passkey/add/finish";
const REMOVE: &str = "/api/auth/passkey/remove";
const ME: &str = "/api/auth/me";
const SIGN_OUT: &str = "/api/auth/sign-out";
const GOOGLE_BEGIN: &str = "/api/auth/google/begin";
const GOOGLE_FINISH: &str = "/api/auth/google/finish";
const GOOGLE_UNLINK: &str = "/api/auth/google/unlink";

/// Signs up with a new passkey; returns the account and the credential id.
async fn sign_up(browser: &mut Browser, passkey: &mut Passkey, name: &str) -> (Me, Vec<u8>) {
    let ccr: CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": name }))
        .await
        .unwrap();
    let credential = passkey.register(ccr);
    let credential_id = credential.raw_id.to_vec();
    let me: Me = browser
        .call(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap();
    (me, credential_id)
}

async fn sign_in_assertion(
    browser: &mut Browser,
    passkey: &mut Passkey,
    credential_id: &[u8],
) -> PublicKeyCredential {
    let rcr: RequestChallengeResponse = browser.call(SIGN_IN_BEGIN, json!({})).await.unwrap();
    passkey.sign_in(rcr, credential_id)
}

async fn me(browser: &mut Browser) -> Result<Me, super::test_support::ApiError> {
    browser.call(ME, json!({})).await
}

async fn session_rows(db: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sessions")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn sign_up_then_sign_out_then_sign_in_with_the_passkey(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();

    let (me1, credential_id) = sign_up(&mut browser, &mut passkey, " Jules ").await;
    assert_eq!(me1.display_name.as_deref(), Some("Jules"));
    assert_eq!(me1.passkeys.len(), 1);
    assert!(!me1.google_linked);
    assert_eq!(me(&mut browser).await.unwrap(), me1);

    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();
    assert!(browser.cookie.is_none(), "sign-out clears the cookie");
    assert_eq!(
        me(&mut browser).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );

    let assertion = sign_in_assertion(&mut browser, &mut passkey, &credential_id).await;
    let me2: Me = browser
        .call(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap();
    assert_eq!(me2.user_id, me1.user_id);
    assert!(me2.passkeys[0].last_used_at.is_some());

    // The counter moved forward and was stored.
    let count: i64 = sqlx::query_scalar("SELECT sign_count FROM passkeys")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(count >= 1, "{count}");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_session_id_changes_on_sign_in(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();

    // Sign-up begin creates an anonymous session: the id an attacker could have planted.
    let ccr: CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": "" }))
        .await
        .unwrap();
    let planted = browser.cookie.clone().expect("anonymous session cookie");
    let credential = passkey.register(ccr);
    let _: Me = browser
        .call(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap();
    let signed_in = browser.cookie.clone().expect("signed-in cookie");
    assert_ne!(planted, signed_in, "sign-in must rotate the session id");

    // The planted id is dead: it neither authenticates nor exists any more.
    let mut attacker = app.browser();
    attacker.cookie = Some(planted);
    assert_eq!(
        me(&mut attacker).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(session_rows(&db).await, 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn session_ids_are_stored_hashed(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let cookie = browser.cookie.clone().unwrap();
    let rows: Vec<(Vec<u8>, Value)> = sqlx::query_as("SELECT id_hash, data FROM sessions")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.len(), 32);
    let value = cookie.split_once('=').unwrap().1;
    assert!(!String::from_utf8_lossy(&rows[0].0).contains(value));
    assert!(!rows[0].1.to_string().contains(value));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_replayed_sign_in_is_rejected(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();
    let (_, credential_id) = sign_up(&mut browser, &mut passkey, "a").await;
    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();

    let assertion = sign_in_assertion(&mut browser, &mut passkey, &credential_id).await;
    let anonymous = browser.clone();
    let _: Me = browser
        .call(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap();

    // Same assertion again, in the now signed-in session: the ceremony is gone.
    let error = browser
        .call::<Me>(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);

    // Same assertion with the pre-sign-in cookie: that session was deleted on rotation.
    let mut replay = anonymous;
    let error = replay
        .call::<Me>(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);

    // Same assertion against a fresh challenge: the signed challenge does not match.
    let mut other = app.browser();
    let _: RequestChallengeResponse = other.call(SIGN_IN_BEGIN, json!({})).await.unwrap();
    let error = other
        .call::<Me>(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        me(&mut other).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn two_concurrent_finishes_of_one_ceremony_cannot_both_succeed(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();
    let (_, credential_id) = sign_up(&mut browser, &mut passkey, "a").await;
    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();

    let assertion = sign_in_assertion(&mut browser, &mut passkey, &credential_id).await;
    let (mut a, mut b) = (browser.clone(), browser.clone());
    let body = json!({ "credential": assertion });
    let (ra, rb) = tokio::join!(
        a.call::<Me>(SIGN_IN_FINISH, body.clone()),
        b.call::<Me>(SIGN_IN_FINISH, body.clone())
    );
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_sign_up_ceremony_is_single_use(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();
    let ccr: CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": "a" }))
        .await
        .unwrap();
    let credential = passkey.register(ccr);
    let before = browser.clone();
    let _: Me = browser
        .call(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap();
    let mut replay = before;
    let error = replay
        .call::<Me>(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_expired_ceremony_is_rejected(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::new();
    let ccr: CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": "a" }))
        .await
        .unwrap();
    sqlx::query("UPDATE auth_ceremonies SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let credential = passkey.register(ccr);
    let error = browser
        .call::<Me>(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn user_verification_is_required(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut passkey = Passkey::with_uv(false);
    let ccr: CreationChallengeResponse = browser
        .call(SIGN_UP_BEGIN, json!({ "display_name": "a" }))
        .await
        .unwrap();
    let credential = passkey.register(ccr);
    let error = browser
        .call::<Me>(SIGN_UP_FINISH, json!({ "credential": credential }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_user_handle_pointing_at_another_account_is_rejected(db: PgPool) {
    let app = TestApp::new(db).await;
    let (mut alice, mut bob) = (app.browser(), app.browser());
    let mut bob_key = Passkey::new();
    let (alice_me, _) = sign_up(&mut alice, &mut Passkey::new(), "alice").await;
    let (_, bob_cred) = sign_up(&mut bob, &mut bob_key, "bob").await;
    let () = bob.call(SIGN_OUT, json!({})).await.unwrap();

    // Bob signs with his own passkey but claims Alice's user handle.
    let mut assertion = sign_in_assertion(&mut bob, &mut bob_key, &bob_cred).await;
    assertion.response.user_handle = Some(alice_me.user_id.as_uuid().as_bytes().to_vec().into());
    let error = bob
        .call::<Me>(SIGN_IN_FINISH, json!({ "credential": assertion }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        me(&mut bob).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_expired_session_is_401(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let error = me(&mut browser).await.unwrap_err();
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error.message, "Please sign in.");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_session_past_the_absolute_timeout_is_401_and_deleted(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let long_ago = super::session::now_unix() - 31 * 24 * 60 * 60;
    sqlx::query(
        "UPDATE sessions SET data = jsonb_set(data, '{auth.signed_in_at}', to_jsonb($1::bigint))",
    )
    .bind(long_ago)
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(
        me(&mut browser).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(session_rows(&db).await, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn activity_pushes_back_the_idle_expiry(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let two_hours_ago = super::session::now_unix() - 2 * 60 * 60;
    sqlx::query(
        "UPDATE sessions SET expires_at = now() + interval '1 hour',
         data = jsonb_set(data, '{auth.last_seen_at}', to_jsonb($1::bigint))",
    )
    .bind(two_hours_ago)
    .execute(&db)
    .await
    .unwrap();
    me(&mut browser).await.unwrap();
    let expires_in_days: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM expires_at - now()) / 86400)::float8 FROM sessions",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(expires_in_days > 13.9, "{expires_in_days}");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn sign_out_deletes_the_session_server_side(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let stolen = browser.cookie.clone();
    assert_eq!(session_rows(&db).await, 1);
    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();
    assert_eq!(session_rows(&db).await, 0);
    // A copy of the old cookie is worthless.
    let mut thief = app.browser();
    thief.cookie = stolen;
    assert_eq!(
        me(&mut thief).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_request_loaded_before_sign_out_cannot_resurrect_the_session(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let mut other_tab = browser.clone();
    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();
    // A request from another tab with the old cookie writes to the session (idle touch)...
    sqlx::query("SELECT 1").execute(&db).await.unwrap();
    let _ = me(&mut other_tab).await;
    // ...and must not recreate it.
    assert_eq!(session_rows(&db).await, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn cross_site_posts_are_refused_without_side_effects(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;

    for (origin, site) in [
        (Some("https://evil.example"), Some("cross-site")),
        (Some("https://evil.example"), None),
        (None, Some("cross-site")),
        (None, Some("same-site")),
        (None, None),
        (Some("null"), None),
    ] {
        let mut attacker = browser.clone();
        attacker.origin = origin.map(str::to_owned);
        attacker.fetch_site = site.map(str::to_owned);
        let error = attacker.call::<()>(SIGN_OUT, json!({})).await.unwrap_err();
        assert_eq!(error.status, StatusCode::FORBIDDEN, "{origin:?} {site:?}");
        assert_eq!(session_rows(&db).await, 1, "{origin:?} {site:?}");
    }
    me(&mut browser).await.unwrap();
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn add_list_and_remove_passkeys_but_never_the_last_way_in(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let mut phone = Passkey::new();
    let (me1, _) = sign_up(&mut browser, &mut phone, "a").await;

    // The only passkey cannot be removed.
    let error = browser
        .call::<Me>(REMOVE, json!({ "passkey_id": me1.passkeys[0].id }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);

    // Add a second one.
    let mut laptop = Passkey::new();
    let ccr: CreationChallengeResponse = browser.call(ADD_BEGIN, json!({})).await.unwrap();
    assert_eq!(
        ccr.public_key.exclude_credentials.as_ref().map(Vec::len),
        Some(1),
        "the existing passkey is excluded"
    );
    let credential: RegisterPublicKeyCredential = laptop.register(ccr);
    let me2: Me = browser
        .call(
            ADD_FINISH,
            json!({ "credential": credential, "nickname": "Laptop" }),
        )
        .await
        .unwrap();
    assert_eq!(me2.passkeys.len(), 2);
    assert_eq!(me2.passkeys[1].nickname, "Laptop");

    // Now the first can go, then the second cannot.
    let me3: Me = browser
        .call(REMOVE, json!({ "passkey_id": me2.passkeys[0].id }))
        .await
        .unwrap();
    assert_eq!(me3.passkeys.len(), 1);
    let error = browser
        .call::<Me>(REMOVE, json!({ "passkey_id": me3.passkeys[0].id }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_user_cannot_remove_someone_elses_passkey(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let (mut alice, mut bob) = (app.browser(), app.browser());
    let (alice_me, _) = sign_up(&mut alice, &mut Passkey::new(), "alice").await;
    sign_up(&mut bob, &mut Passkey::new(), "bob").await;
    // Give Alice a second passkey so the last-method rule is not what stops Bob.
    sqlx::query(
        "INSERT INTO passkeys (user_id, credential_id, passkey, backup_eligible, backup_state, nickname)
         SELECT user_id, '\\x01'::bytea, passkey, false, false, 'copy' FROM passkeys WHERE user_id = $1",
    )
    .bind(alice_me.user_id.as_uuid())
    .execute(&db)
    .await
    .unwrap();
    let error = bob
        .call::<Me>(REMOVE, json!({ "passkey_id": alice_me.passkeys[0].id }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(me(&mut alice).await.unwrap().passkeys.len(), 2);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn signed_out_calls_to_protected_functions_are_401(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    for path in [ME, ADD_BEGIN, GOOGLE_UNLINK] {
        let error = browser.call::<Value>(path, json!({})).await.unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let error = browser
        .call::<Value>(REMOVE, json!({ "passkey_id": uuid::Uuid::nil() }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    let error = browser
        .call::<Value>(GOOGLE_BEGIN, json!({ "intent": "Link" }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn deleting_a_user_deletes_their_auth_rows(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    let (me1, _) = sign_up(&mut browser, &mut Passkey::new(), "a").await;
    google_sign_in_or_link(&app, &mut browser, "Link", "sub-cascade")
        .await
        .unwrap();
    let _: RequestChallengeResponse = browser.call(SIGN_IN_BEGIN, json!({})).await.unwrap();
    let _: CreationChallengeResponse = browser.call(ADD_BEGIN, json!({})).await.unwrap();

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(me1.user_id.as_uuid())
        .execute(&db)
        .await
        .unwrap();
    for table in ["passkeys", "oauth_identities", "sessions"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let bound: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth_ceremonies WHERE user_id IS NOT NULL")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(bound, 0);
    assert_eq!(
        me(&mut browser).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
}

// --- Google --------------------------------------------------------------------------------

/// Starts a Google flow and returns the authorization URL.
async fn google_begin(browser: &mut Browser, intent: &str) -> String {
    browser
        .call(GOOGLE_BEGIN, json!({ "intent": intent }))
        .await
        .unwrap()
}

fn callback_path(code: &str, state: &str) -> String {
    format!(
        "/auth/google/callback?code={}&state={}",
        url::form_urlencoded::byte_serialize(code.as_bytes()).collect::<String>(),
        url::form_urlencoded::byte_serialize(state.as_bytes()).collect::<String>(),
    )
}

/// Runs a whole Google flow (callback in the same browser) for `subject`; returns the page's
/// message type.
async fn google_sign_in_or_link(
    app: &TestApp,
    browser: &mut Browser,
    intent: &str,
    subject: &str,
) -> Result<(), String> {
    let url = google_begin(browser, intent).await;
    let claims = app.google.claims(&url, subject);
    app.google.grant(
        "code-1",
        Grant {
            code_challenge: query_param(&url, "code_challenge"),
            claims,
            sign_with_unpublished_key: false,
        },
    );
    let (status, _, body) = browser
        .get(&callback_path("code-1", &query_param(&url, "state")))
        .await;
    assert_eq!(status, StatusCode::OK);
    if body.contains(r#""type":"done""#) {
        Ok(())
    } else {
        Err(body)
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_authorization_url_uses_pkce_state_and_nonce(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    let url = google_begin(&mut browser, "SignIn").await;
    assert!(
        url.starts_with(&format!("{}/auth?", app.google.issuer)),
        "{url}"
    );
    assert_eq!(query_param(&url, "response_type"), "code");
    assert_eq!(query_param(&url, "code_challenge_method"), "S256");
    assert_eq!(
        query_param(&url, "client_id"),
        super::test_support::CLIENT_ID
    );
    assert_eq!(
        query_param(&url, "redirect_uri"),
        "http://localhost:8080/auth/google/callback"
    );
    assert!(query_param(&url, "scope").split(' ').any(|s| s == "openid"));
    assert!(query_param(&url, "state").len() >= 20);
    assert!(query_param(&url, "nonce").len() >= 20);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_sign_in_creates_then_finds_the_account_by_sub(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    google_sign_in_or_link(&app, &mut browser, "SignIn", "sub-1")
        .await
        .unwrap();
    let first = me(&mut browser).await.unwrap();
    assert!(first.google_linked);
    assert!(first.passkeys.is_empty());

    let () = browser.call(SIGN_OUT, json!({})).await.unwrap();
    let before = browser.cookie.clone();
    google_sign_in_or_link(&app, &mut browser, "SignIn", "sub-1")
        .await
        .unwrap();
    assert_ne!(browser.cookie, before, "the session id rotates on sign-in");
    assert_eq!(me(&mut browser).await.unwrap().user_id, first.user_id);

    // Another subject with the same email is another account: never linked by email.
    let mut other = app.browser();
    google_sign_in_or_link(&app, &mut other, "SignIn", "sub-2")
        .await
        .unwrap();
    assert_ne!(me(&mut other).await.unwrap().user_id, first.user_id);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_popup_without_the_session_hands_the_code_to_the_opener(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut opener = app.browser();
    let url = google_begin(&mut opener, "SignIn").await;
    let state = query_param(&url, "state");
    app.google.grant(
        "code-2",
        Grant {
            code_challenge: query_param(&url, "code_challenge"),
            claims: app.google.claims(&url, "sub-popup"),
            sign_with_unpublished_key: false,
        },
    );

    // The popup has its own (empty) cookie jar: it cannot finish, it relays.
    let mut popup = app.browser();
    popup.origin = None;
    popup.fetch_site = Some("cross-site".to_owned());
    let (status, headers, body) = popup.get(&callback_path("code-2", &state)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert!(body.contains(r#""type":"code""#), "{body}");
    assert!(
        body.contains(r#""origin":"http://localhost:8080""#),
        "{body}"
    );
    assert!(popup.cookie.is_none(), "the popup gets no session");

    let me: Me = opener
        .call(GOOGLE_FINISH, json!({ "code": "code-2", "state": state }))
        .await
        .unwrap();
    assert!(me.google_linked);
}

/// Begins a flow and grants a code with `tamper` applied; returns the finish error status.
async fn google_finish_with(
    app: &TestApp,
    tamper: impl FnOnce(&mut Grant, &mut String),
) -> StatusCode {
    let mut browser = app.browser();
    let url = google_begin(&mut browser, "SignIn").await;
    let mut state = query_param(&url, "state");
    let mut grant = Grant {
        code_challenge: query_param(&url, "code_challenge"),
        claims: app.google.claims(&url, "sub-tamper"),
        sign_with_unpublished_key: false,
    };
    tamper(&mut grant, &mut state);
    app.google.grant("code-t", grant);
    let result = browser
        .call::<Me>(GOOGLE_FINISH, json!({ "code": "code-t", "state": state }))
        .await;
    let status = match result {
        Ok(_) => StatusCode::OK,
        Err(error) => error.status,
    };
    if status != StatusCode::OK {
        assert_eq!(
            me(&mut browser).await.unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
    }
    status
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_state_mismatch(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let status = google_finish_with(&app, |_, state| state.push('x')).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_nonce_mismatch(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| {
        grant.claims["nonce"] = json!("another-nonce");
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_token_for_another_client(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| {
        grant.claims["aud"] = json!("someone-else.apps.googleusercontent.com");
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_token_from_another_issuer(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| {
        grant.claims["iss"] = json!("https://evil.example");
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_an_expired_token(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| {
        let now = super::session::now_unix();
        grant.claims["iat"] = json!(now - 7200);
        grant.claims["exp"] = json!(now - 3600);
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_token_signed_with_an_unknown_key(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| grant.sign_with_unpublished_key = true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_rejects_a_code_bound_to_another_pkce_challenge(db: PgPool) {
    let app = TestApp::new(db).await;
    let status = google_finish_with(&app, |grant, _| {
        grant.code_challenge = "not-our-challenge".to_owned();
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_google_ceremony_is_single_use(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    let url = google_begin(&mut browser, "SignIn").await;
    let state = query_param(&url, "state");
    for code in ["c1", "c2"] {
        app.google.grant(
            code,
            Grant {
                code_challenge: query_param(&url, "code_challenge"),
                claims: app.google.claims(&url, "sub-once"),
                sign_with_unpublished_key: false,
            },
        );
    }
    let before = browser.clone();
    let _: Me = browser
        .call(GOOGLE_FINISH, json!({ "code": "c1", "state": state }))
        .await
        .unwrap();
    let mut replay = before;
    let error = replay
        .call::<Me>(GOOGLE_FINISH, json!({ "code": "c2", "state": state }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_forged_callback_cannot_log_the_victim_into_the_attackers_account(db: PgPool) {
    // Login CSRF: the attacker gets a code for their own Google account and sends the victim
    // to the callback with it.
    let app = TestApp::new(db).await;
    let mut attacker = app.browser();
    let url = google_begin(&mut attacker, "SignIn").await;
    app.google.grant(
        "attacker-code",
        Grant {
            code_challenge: query_param(&url, "code_challenge"),
            claims: app.google.claims(&url, "attacker-sub"),
            sign_with_unpublished_key: false,
        },
    );
    let mut victim = app.browser();
    // The victim has a Google flow of their own in flight (worst case).
    let _ = google_begin(&mut victim, "SignIn").await;
    let (status, _, body) = victim
        .get(&callback_path("attacker-code", &query_param(&url, "state")))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""type":"error""#), "{body}");
    assert_eq!(
        me(&mut victim).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn linking_cannot_take_over_another_accounts_google(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    // Alice signs up with Google.
    let mut alice = app.browser();
    google_sign_in_or_link(&app, &mut alice, "SignIn", "alice-sub")
        .await
        .unwrap();
    let alice_me = me(&mut alice).await.unwrap();

    // Bob (passkey account) tries to link Alice's Google account.
    let mut bob = app.browser();
    let (bob_me, _) = sign_up(&mut bob, &mut Passkey::new(), "bob").await;
    let body = google_sign_in_or_link(&app, &mut bob, "Link", "alice-sub")
        .await
        .unwrap_err();
    assert!(body.contains("already used by another account"), "{body}");
    assert!(!me(&mut bob).await.unwrap().google_linked);

    // Signing in with that Google account is still Alice, never Bob.
    let mut again = app.browser();
    google_sign_in_or_link(&app, &mut again, "SignIn", "alice-sub")
        .await
        .unwrap();
    let who = me(&mut again).await.unwrap().user_id;
    assert_eq!(who, alice_me.user_id);
    assert_ne!(who, bob_me.user_id);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn link_then_sign_in_with_google_and_unlink_rules(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    let (me1, _) = sign_up(&mut browser, &mut Passkey::new(), "a").await;
    google_sign_in_or_link(&app, &mut browser, "Link", "sub-link")
        .await
        .unwrap();
    let linked = me(&mut browser).await.unwrap();
    assert!(linked.google_linked);
    assert_eq!(linked.sign_in_methods(), 2);

    // Linking a second, different Google account is refused.
    let body = google_sign_in_or_link(&app, &mut browser, "Link", "sub-other")
        .await
        .unwrap_err();
    assert!(body.contains("different Google account"), "{body}");

    // Sign in with the linked Google account: same user.
    let mut phone = app.browser();
    google_sign_in_or_link(&app, &mut phone, "SignIn", "sub-link")
        .await
        .unwrap();
    assert_eq!(me(&mut phone).await.unwrap().user_id, me1.user_id);

    // Unlink, then removing the last passkey is refused.
    let unlinked: Me = browser.call(GOOGLE_UNLINK, json!({})).await.unwrap();
    assert!(!unlinked.google_linked);
    let error = browser
        .call::<Me>(REMOVE, json!({ "passkey_id": unlinked.passkeys[0].id }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_as_the_only_method_cannot_be_unlinked(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut browser = app.browser();
    google_sign_in_or_link(&app, &mut browser, "SignIn", "sub-only")
        .await
        .unwrap();
    let error = browser
        .call::<Me>(GOOGLE_UNLINK, json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_link_ceremony_cannot_be_finished_by_another_user(db: PgPool) {
    let app = TestApp::new(db).await;
    let mut alice = app.browser();
    sign_up(&mut alice, &mut Passkey::new(), "alice").await;
    let url = google_begin(&mut alice, "Link").await;
    let state = query_param(&url, "state");
    app.google.grant(
        "code-l",
        Grant {
            code_challenge: query_param(&url, "code_challenge"),
            claims: app.google.claims(&url, "sub-l"),
            sign_with_unpublished_key: false,
        },
    );
    // Alice signs out and Bob signs in on the same browser before the callback.
    let () = alice.call(SIGN_OUT, json!({})).await.unwrap();
    assert!(alice.cookie.is_none());
    let mut bob = alice.clone();
    sign_up(&mut bob, &mut Passkey::new(), "bob").await;
    let error = bob
        .call::<Me>(GOOGLE_FINISH, json!({ "code": "code-l", "state": state }))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(!me(&mut bob).await.unwrap().google_linked);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn cleanup_deletes_expired_sessions_and_ceremonies(db: PgPool) {
    let app = TestApp::new(db.clone()).await;
    let mut browser = app.browser();
    sign_up(&mut browser, &mut Passkey::new(), "a").await;
    let _: RequestChallengeResponse = app.browser().call(SIGN_IN_BEGIN, json!({})).await.unwrap();
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE auth_ceremonies SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let deleted = super::session::PgSessionStore::new(db.clone())
        .delete_expired()
        .await
        .unwrap();
    assert_eq!(deleted, 3, "two sessions and one ceremony");
    assert_eq!(session_rows(&db).await, 0);
}
