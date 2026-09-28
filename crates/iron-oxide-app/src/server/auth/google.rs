//! Sign in with Google: OpenID Connect authorization code flow with PKCE, `state` and `nonce`.
//!
//! 1. [`begin`] (a POST server function) discovers Google's endpoints, stores a ceremony with a
//!    random `state`, `nonce` and PKCE verifier, and returns the authorization URL. The client
//!    opens it in a popup (started synchronously from the tap, for iOS) or, if popups are
//!    blocked, in the current window.
//! 2. Google redirects to `GET /auth/google/callback?code&state` ([`callback`]).
//!    - If this request carries the session that started the flow (full redirect, or a popup
//!      sharing the app's cookies), the callback finishes the flow itself and tells the opener
//!      it is done (or redirects to `/`).
//!    - Otherwise (an iOS standalone PWA popup may have its own cookie jar), the page hands
//!      `code` and `state` to the opener with `postMessage` restricted to our origin, and the
//!      opener finishes with the `google_finish` server function in its own session. The code
//!      is useless to anyone else: it needs the PKCE verifier, which never leaves the server.
//! 3. [`finish`] checks `state` (constant time) against the ceremony, exchanges the code with
//!    the PKCE verifier, and verifies the ID token: signature against Google's current JWKS
//!    (fetched per sign-in, so key rotation needs no restart), issuer, audience (our client id),
//!    expiry and nonce. The account is found by the `sub` claim only, never by email.

use std::time::Duration;

use dioxus::logger::tracing;
use dioxus::server::axum::{
    extract::Query,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    core::{CoreAuthPrompt, CoreProviderMetadata, CoreResponseType},
    reqwest,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{
    AuthContext,
    ceremony::{self, CeremonyKind},
    error::AuthError,
    passkeys::lock_user_and_count_methods,
};
use crate::auth::types::{
    GOOGLE_CALLBACK_CHANNEL, GoogleCallbackMessage, GoogleIntent, Me, UserId,
};

/// Google's OpenID Connect issuer.
pub const GOOGLE_ISSUER: &str = "https://accounts.google.com";
/// Timeout for each request to Google.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest `code` or `state` accepted from the callback.
const MAX_PARAM_LEN: usize = 2048;

/// The Google OAuth client: issuer, credentials, redirect URL and HTTP client.
#[derive(Debug, Clone)]
pub struct GoogleOidc {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: SecretString,
    redirect_url: RedirectUrl,
    http: reqwest::Client,
}

impl GoogleOidc {
    /// A client for `issuer` (Google's in production; a local mock in tests).
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: SecretString,
        redirect_url: &url::Url,
    ) -> Result<Self, AuthError> {
        let http = reqwest::Client::builder()
            // No redirects: the discovery, JWKS and token endpoints answer directly.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| AuthError::Internal(format!("HTTP client: {e}")))?;
        Ok(Self {
            issuer: IssuerUrl::new(issuer.to_owned())
                .map_err(|e| AuthError::Internal(format!("issuer URL: {e}")))?,
            client_id: ClientId::new(client_id.to_owned()),
            client_secret,
            redirect_url: RedirectUrl::from_url(redirect_url.clone()),
            http,
        })
    }

    /// Discovers the endpoints and current signing keys, and builds the OIDC client.
    async fn client(
        &self,
    ) -> Result<
        openidconnect::core::CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        AuthError,
    > {
        let metadata = CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http)
            .await
            .map_err(|e| AuthError::Google(format!("discovery: {e}")))?;
        Ok(openidconnect::core::CoreClient::from_provider_metadata(
            metadata,
            self.client_id.clone(),
            Some(ClientSecret::new(
                self.client_secret.expose_secret().to_owned(),
            )),
        )
        .set_redirect_uri(self.redirect_url.clone()))
    }
}

/// What the Google ceremony remembers between begin and finish.
#[derive(Serialize, Deserialize)]
struct GoogleState {
    state: String,
    nonce: String,
    pkce_verifier: String,
}

/// Starts a Google sign-in (or link) and returns the authorization URL.
pub async fn begin(ctx: &AuthContext, intent: GoogleIntent) -> Result<String, AuthError> {
    let (kind, user) = match intent {
        GoogleIntent::SignIn => (CeremonyKind::GoogleSignIn, None),
        GoogleIntent::Link => (CeremonyKind::GoogleLink, Some(ctx.require_user().await?)),
    };
    let client = ctx.auth.google().client().await?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = client
        .authorize_url(
            AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(challenge)
        // Let the user pick the account, rather than silently reusing the last one.
        .add_prompt(CoreAuthPrompt::SelectAccount)
        .url();
    let state = GoogleState {
        state: state.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
    };
    ceremony::start(ctx.db(), &ctx.session, kind, user, &state).await?;
    Ok(url.to_string())
}

/// Whether the session has a Google ceremony in flight.
pub async fn has_ceremony(ctx: &AuthContext) -> Result<bool, AuthError> {
    Ok(ctx
        .session
        .get::<uuid::Uuid>(CeremonyKind::GoogleSignIn.session_key())
        .await?
        .is_some())
}

/// Constant-time comparison of the returned `state` with the stored one.
fn state_matches(expected: &str, actual: &str) -> bool {
    expected.len() == actual.len() && bool::from(expected.as_bytes().ct_eq(actual.as_bytes()))
}

fn check_param(value: &str) -> Result<(), AuthError> {
    if value.is_empty() || value.len() > MAX_PARAM_LEN {
        return Err(AuthError::Google(
            "missing or oversized code/state".to_owned(),
        ));
    }
    Ok(())
}

/// Finishes the Google flow started in this session: signs in (creating the account on first
/// use), or links Google to the signed-in user.
pub async fn finish(ctx: &AuthContext, code: &str, state: &str) -> Result<(), AuthError> {
    check_param(code)?;
    check_param(state)?;
    let current = ctx.current_user().await?;
    let taken = ceremony::take_google(ctx.db(), &ctx.session, current).await?;
    let (kind, owner) = (taken.kind, taken.user);
    let stored: GoogleState = taken.state()?;
    if !state_matches(&stored.state, state) {
        return Err(AuthError::Ceremony("state mismatch"));
    }

    let client = ctx.auth.google().client().await?;
    let token = client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .map_err(|e| AuthError::Google(format!("token endpoint: {e}")))?
        .set_pkce_verifier(PkceCodeVerifier::new(stored.pkce_verifier))
        .request_async(&ctx.auth.google().http)
        .await
        .map_err(|e| AuthError::Google(format!("code exchange: {e}")))?;
    let id_token = openidconnect::TokenResponse::id_token(&token)
        .ok_or_else(|| AuthError::Google("no ID token".to_owned()))?;
    let claims = id_token
        .claims(&client.id_token_verifier(), &Nonce::new(stored.nonce))
        .map_err(|e| AuthError::Google(format!("ID token: {e}")))?;
    let subject = claims.subject().as_str();

    match (kind, owner) {
        (CeremonyKind::GoogleLink, Some(user)) => link(ctx, user, subject).await,
        (CeremonyKind::GoogleSignIn, None) => {
            let user = sign_in_or_create(ctx, subject).await?;
            ctx.sign_in(user).await
        }
        _ => Err(AuthError::Internal(format!(
            "unexpected Google ceremony {kind:?}"
        ))),
    }
}

/// The user linked to this Google subject, or a new user with it.
async fn sign_in_or_create(ctx: &AuthContext, subject: &str) -> Result<UserId, AuthError> {
    if let Some(user) = sqlx::query_scalar!(
        "UPDATE oauth_identities SET last_used_at = now()
         WHERE provider = 'google' AND subject = $1
         RETURNING user_id",
        subject
    )
    .fetch_optional(ctx.db())
    .await?
    {
        return Ok(UserId::from_uuid(user));
    }

    let mut tx = ctx.db().begin().await?;
    let user = sqlx::query_scalar!("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(&mut *tx)
        .await?;
    let linked = sqlx::query_scalar!(
        "INSERT INTO oauth_identities (user_id, provider, subject, last_used_at)
         VALUES ($1, 'google', $2, now())
         ON CONFLICT (provider, subject) DO NOTHING
         RETURNING user_id",
        user,
        subject
    )
    .fetch_optional(&mut *tx)
    .await?;
    match linked {
        Some(user) => {
            tx.commit().await?;
            Ok(UserId::from_uuid(user))
        }
        None => {
            // A concurrent first sign-in created it: use that account, drop ours.
            tx.rollback().await?;
            sqlx::query_scalar!(
                "SELECT user_id FROM oauth_identities WHERE provider = 'google' AND subject = $1",
                subject
            )
            .fetch_optional(ctx.db())
            .await?
            .map(UserId::from_uuid)
            .ok_or_else(|| AuthError::Internal("Google identity vanished".to_owned()))
        }
    }
}

/// Links the Google `subject` to `user`. Idempotent for the same pair; refused if the subject
/// belongs to someone else or the user already has a different Google account.
async fn link(ctx: &AuthContext, user: UserId, subject: &str) -> Result<(), AuthError> {
    let mut tx = ctx.db().begin().await?;
    lock_user_and_count_methods(&mut tx, user).await?;
    let owner = sqlx::query_scalar!(
        "SELECT user_id FROM oauth_identities WHERE provider = 'google' AND subject = $1",
        subject
    )
    .fetch_optional(&mut *tx)
    .await?;
    match owner {
        Some(owner) if owner == user.as_uuid() => return Ok(()),
        Some(_) => return Err(AuthError::GoogleLinkedElsewhere),
        None => {}
    }
    sqlx::query!(
        "INSERT INTO oauth_identities (user_id, provider, subject) VALUES ($1, 'google', $2)",
        user.as_uuid(),
        subject
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) => match db.constraint() {
            Some("oauth_identities_user_id_provider_key") => AuthError::GoogleAlreadyLinked,
            Some("oauth_identities_provider_subject_key") => AuthError::GoogleLinkedElsewhere,
            _ => AuthError::Database(error),
        },
        _ => AuthError::Database(error),
    })?;
    tx.commit().await?;
    Ok(())
}

/// Unlinks Google from the user, unless it is their last way to sign in.
pub async fn unlink(ctx: &AuthContext, user: UserId) -> Result<Me, AuthError> {
    let mut tx = ctx.db().begin().await?;
    let methods = lock_user_and_count_methods(&mut tx, user).await?;
    let deleted = sqlx::query!(
        "DELETE FROM oauth_identities WHERE user_id = $1 AND provider = 'google'",
        user.as_uuid()
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(AuthError::NotFound);
    }
    if methods <= 1 {
        return Err(AuthError::LastSignInMethod);
    }
    tx.commit().await?;
    super::passkeys::me(ctx, user).await
}

/// Query parameters of the callback.
#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /auth/google/callback`: see the module docs.
pub async fn callback(ctx: AuthContext, Query(params): Query<CallbackParams>) -> Response {
    let origin = ctx.auth.origin().to_owned();
    let (message, redirect_home) = match callback_outcome(&ctx, params).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = GoogleCallbackMessage::Error {
                message: error.public().1.to_owned(),
            };
            tracing::warn!(%error, "Google callback failed");
            (message, false)
        }
    };
    callback_page(&origin, &message, redirect_home)
}

async fn callback_outcome(
    ctx: &AuthContext,
    params: CallbackParams,
) -> Result<(GoogleCallbackMessage, bool), AuthError> {
    if let Some(error) = params.error {
        // e.g. `access_denied` when the user cancels. Clear the ceremony.
        if has_ceremony(ctx).await? {
            let current = ctx.current_user().await?;
            let _ = ceremony::take_google(ctx.db(), &ctx.session, current).await;
        }
        return Err(AuthError::Google(format!("Google returned {error:.64}")));
    }
    let (Some(code), Some(state)) = (params.code, params.state) else {
        return Err(AuthError::Google(
            "callback without code or state".to_owned(),
        ));
    };
    if has_ceremony(ctx).await? {
        finish(ctx, &code, &state).await?;
        Ok((GoogleCallbackMessage::Done, true))
    } else {
        check_param(&code)?;
        check_param(&state)?;
        Ok((GoogleCallbackMessage::Code { code, state }, false))
    }
}

/// The callback page's script. Static, so the CSP can allow exactly it by hash.
const CALLBACK_SCRIPT: &str = r#"(function () {
  var data = JSON.parse(document.getElementById("data").textContent);
  var message = JSON.stringify(data.message);
  var delivered = false;
  try {
    if (window.opener && window.opener !== window) {
      window.opener.postMessage(message, data.origin);
      delivered = true;
    }
  } catch (e) {}
  try {
    var channel = new BroadcastChannel(data.channel);
    channel.postMessage(message);
    channel.close();
  } catch (e) {}
  if (delivered) {
    window.close();
  } else if (data.redirect) {
    window.location.replace("/");
  }
})();"#;

/// JSON that is safe inside a `<script>` element: no `<`, `>` or `&`.
fn script_safe_json(value: &serde_json::Value) -> String {
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Builds the callback page. Never cached, never sends a referrer (the URL holds the code),
/// cannot be framed, and runs only its own script.
fn callback_page(origin: &str, message: &GoogleCallbackMessage, redirect_home: bool) -> Response {
    let data = serde_json::json!({
        "origin": origin,
        "channel": GOOGLE_CALLBACK_CHANNEL,
        "message": message,
        "redirect": redirect_home,
    });
    let text = match message {
        GoogleCallbackMessage::Done => "Signed in. You can close this window.".to_owned(),
        GoogleCallbackMessage::Code { .. } => {
            "Almost done. Return to Iron Oxide to finish signing in.".to_owned()
        }
        GoogleCallbackMessage::Error { message } => message.clone(),
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Iron Oxide</title></head><body>\
         <p>{}</p><p><a href=\"/\">Back to Iron Oxide</a></p>\
         <script type=\"application/json\" id=\"data\">{}</script>\
         <script>{CALLBACK_SCRIPT}</script></body></html>",
        html_escape(&text),
        script_safe_json(&data),
    );
    let script_hash = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        Sha256::digest(CALLBACK_SCRIPT.as_bytes()),
    );
    let csp = format!(
        "default-src 'none'; script-src 'sha256-{script_hash}'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'"
    );
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(csp) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_comparison() {
        assert!(state_matches("abc", "abc"));
        assert!(!state_matches("abc", "abd"));
        assert!(!state_matches("abc", "abcd"));
        assert!(!state_matches("abc", ""));
    }

    #[test]
    fn params_must_be_present_and_bounded() {
        assert!(check_param("x").is_ok());
        assert!(check_param(&"x".repeat(MAX_PARAM_LEN)).is_ok());
        assert!(check_param("").is_err());
        assert!(check_param(&"x".repeat(MAX_PARAM_LEN + 1)).is_err());
    }

    #[test]
    fn script_safe_json_cannot_close_the_script_element() {
        let value = serde_json::json!({ "code": "</script><script>alert(1)</script>&" });
        let json = script_safe_json(&value);
        assert!(
            !json.contains('<') && !json.contains('>') && !json.contains('&'),
            "{json}"
        );
        // Still the same JSON once parsed.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            value
        );
    }

    #[test]
    fn html_escape_escapes_markup() {
        assert_eq!(
            html_escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }

    async fn page_text(response: Response) -> String {
        let body = dioxus::server::axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn callback_page_is_locked_down() {
        let response = callback_page(
            "https://iron-oxyde.com",
            &GoogleCallbackMessage::Code {
                code: "</script>".to_owned(),
                state: "s".to_owned(),
            },
            false,
        );
        let headers = response.headers().clone();
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        let csp = headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            csp.starts_with("default-src 'none'; script-src 'sha256-"),
            "{csp}"
        );
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        let body = page_text(response).await;
        assert_eq!(body.matches("</script>").count(), 2, "{body}");
        assert!(
            body.contains(r#""origin":"https://iron-oxyde.com""#),
            "{body}"
        );
        assert!(body.contains(CALLBACK_SCRIPT));
    }

    #[tokio::test]
    async fn callback_error_page_escapes_the_message() {
        let response = callback_page(
            "http://localhost:8080",
            &GoogleCallbackMessage::Error {
                message: "<b>no</b>".to_owned(),
            },
            false,
        );
        let body = page_text(response).await;
        assert!(body.contains("<p>&lt;b&gt;no&lt;/b&gt;</p>"), "{body}");
    }
}
