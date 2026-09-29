//! Test harness for the sign-in integration tests: the full app router with a cookie-keeping
//! client, a software passkey, and a local mock of Google's OpenID Connect endpoints.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, OnceLock},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use dioxus::server::axum::{
    self, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use openssl::{hash::MessageDigest, pkey::PKey, rsa::Rsa, sign::Signer};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs_proto::{
    AllowCredentials, CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, ResidentKeyRequirement, UserVerificationPolicy,
};

use super::AuthState;
use crate::auth::types::Me;
use crate::server::{AppState, Config, rate_limit::RateLimitConfig, router};

/// The client address browsers connect from unless a test picks another one.
pub const DEFAULT_PEER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

pub const ORIGIN: &str = "http://localhost:8080";
pub const CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "test-client-secret";
// 64 zero bytes, base64-encoded (a test-only session key).
const SESSION_KEY: &str =
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

pub fn config() -> Config {
    Config::from_lookup(|name| {
        let value = match name {
            "DATABASE_URL" => "postgres://u:p@127.0.0.1:1/db",
            "APP_BASE_URL" => ORIGIN,
            "WEBAUTHN_RP_ID" => "localhost",
            "WEBAUTHN_ORIGIN" => ORIGIN,
            "GOOGLE_CLIENT_ID" => CLIENT_ID,
            "GOOGLE_CLIENT_SECRET" => CLIENT_SECRET,
            "GOOGLE_REDIRECT_URL" => "http://localhost:8080/auth/google/callback",
            "SESSION_KEY" => SESSION_KEY,
            _ => return Err(std::env::VarError::NotPresent),
        };
        Ok(value.to_owned())
    })
    .unwrap()
}

/// The Dioxus router serves `public/` next to the executable and panics if it is missing
/// (`dx` creates it in real builds). Tests only call server functions: an empty one will do.
fn ensure_public_dir() {
    let exe = std::env::current_exe().unwrap();
    std::fs::create_dir_all(exe.parent().unwrap().join("public")).unwrap();
}

/// The app under test, with its database and the mock Google.
#[derive(Clone)]
pub struct TestApp {
    pub router: Router,
    pub google: MockGoogle,
}

impl TestApp {
    pub async fn new(db: PgPool) -> Self {
        Self::with_rate_limit(db, RateLimitConfig::default()).await
    }

    /// The app with other rate limits or client-IP source.
    pub async fn with_rate_limit(db: PgPool, rate_limit: RateLimitConfig) -> Self {
        ensure_public_dir();
        let google = MockGoogle::start().await;
        let mut config = config();
        config.rate_limit = rate_limit;
        let auth = AuthState::with_google_issuer(&config, &google.issuer).unwrap();
        let state = AppState {
            config: Arc::new(config),
            db,
        };
        Self {
            router: router(state, auth),
            google,
        }
    }

    /// A browser: its own cookie jar, same-origin headers.
    pub fn browser(&self) -> Browser {
        Browser {
            router: self.router.clone(),
            cookie: None,
            origin: Some(ORIGIN.to_owned()),
            fetch_site: Some("same-origin".to_owned()),
            peer: Some(DEFAULT_PEER),
            headers: Vec::new(),
        }
    }
}

/// A failed server-function call, as decoded from the response: the status and the `error`
/// message of the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub status: StatusCode,
    pub message: String,
}

/// A cookie-keeping HTTP client driving the router in-process.
#[derive(Clone)]
pub struct Browser {
    router: Router,
    /// The session cookie (`name=value`), as the browser would store it.
    pub cookie: Option<String>,
    pub origin: Option<String>,
    pub fetch_site: Option<String>,
    /// The TCP peer address the server sees (`None`: no connection info, as with `oneshot`).
    pub peer: Option<IpAddr>,
    /// Extra headers sent with every request.
    pub headers: Vec<(&'static str, String)>,
}

impl Browser {
    pub async fn send(&mut self, request: Request<Body>) -> Response {
        let response = self.router.clone().oneshot(request).await.unwrap();
        for value in response.headers().get_all(header::SET_COOKIE) {
            let value = value.to_str().unwrap();
            let pair = value.split(';').next().unwrap().trim().to_owned();
            let removed = value.to_ascii_lowercase().contains("max-age=0") || pair.ends_with('=');
            self.cookie = if removed { None } else { Some(pair) };
        }
        response
    }

    /// A request from this browser: its cookie and same-origin headers, no body yet.
    pub fn request(
        &self,
        method: &str,
        path: &str,
    ) -> dioxus::server::axum::http::request::Builder {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost:8080");
        if let Some(cookie) = &self.cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        if let Some(origin) = &self.origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(site) = &self.fetch_site {
            builder = builder.header("sec-fetch-site", site);
        }
        for (name, value) in &self.headers {
            builder = builder.header(*name, value);
        }
        if let Some(peer) = self.peer {
            builder = builder.extension(ConnectInfo(SocketAddr::new(peer, 50_000)));
        }
        builder
    }

    /// `POST path` with a JSON body, returning the status and the raw body.
    pub async fn post_json(&mut self, path: &str, body: Value) -> (StatusCode, Vec<u8>) {
        let request = self
            .request("POST", path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = self.send(request).await;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, bytes.to_vec())
    }

    /// Calls a server function (`POST` with a JSON body) and decodes the result.
    pub async fn call<T: DeserializeOwned>(
        &mut self,
        path: &str,
        body: Value,
    ) -> Result<T, CallError> {
        let (status, bytes) = self.post_json(path, body).await;
        if status.is_success() {
            Ok(serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{path}: {e}: {}", String::from_utf8_lossy(&bytes))))
        } else {
            // `/api/` errors carry our message in `data.ServerError.message` (see
            // `server::api::errors_layer`); other routes send `{"error": message}`.
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v["data"]["ServerError"]["message"]
                        .as_str()
                        .or_else(|| v["error"].as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
            Err(CallError { status, message })
        }
    }

    /// `GET path`, returning status and body.
    pub async fn get(&mut self, path: &str) -> (StatusCode, HeaderMap, String) {
        let request = self.request("GET", path).body(Body::empty()).unwrap();
        let response = self.send(request).await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }
}

/// Signs up with a new passkey on `browser`, which is then signed in as the new user. Returns the
/// account and the credential id.
pub async fn sign_up(browser: &mut Browser, passkey: &mut Passkey, name: &str) -> (Me, Vec<u8>) {
    let ccr: CreationChallengeResponse = browser
        .call(
            "/api/auth/passkey/sign-up/begin",
            json!({ "display_name": name }),
        )
        .await
        .unwrap();
    let credential = passkey.register(ccr);
    let credential_id = credential.raw_id.to_vec();
    let me: Me = browser
        .call(
            "/api/auth/passkey/sign-up/finish",
            json!({ "credential": credential }),
        )
        .await
        .unwrap();
    (me, credential_id)
}

/// A software passkey, adapted to discoverable credentials: `SoftPasskey` supports neither
/// resident keys nor user handles, so the test plays the browser's part for those.
pub struct Passkey {
    authenticator: WebauthnAuthenticator<SoftPasskey>,
    /// Whether the authenticator verifies the user (sets the UV flag).
    uv: bool,
    /// Credential id → user handle, as a real discoverable credential would remember.
    handles: HashMap<Vec<u8>, Vec<u8>>,
}

impl Passkey {
    pub fn new() -> Self {
        Self::with_uv(true)
    }

    /// `uv = false`: an authenticator that does not verify the user (no UV flag).
    pub fn with_uv(uv: bool) -> Self {
        // SoftPasskey has no real user verification: `true` makes it report UV as done, which
        // is what a platform passkey does after Face ID / Touch ID.
        Self {
            authenticator: WebauthnAuthenticator::new(SoftPasskey::new(uv)),
            uv,
            handles: HashMap::new(),
        }
    }

    pub fn register(&mut self, mut ccr: CreationChallengeResponse) -> RegisterPublicKeyCredential {
        let selection = ccr.public_key.authenticator_selection.as_ref().unwrap();
        assert_eq!(
            selection.resident_key,
            Some(ResidentKeyRequirement::Required)
        );
        assert!(selection.require_resident_key);
        // SoftPasskey refuses resident keys; the server does not depend on the flag.
        let user_handle = ccr.public_key.user.id.to_vec();
        if let Some(selection) = ccr.public_key.authenticator_selection.as_mut() {
            selection.resident_key = Some(ResidentKeyRequirement::Discouraged);
            selection.require_resident_key = false;
            if !self.uv {
                selection.user_verification = UserVerificationPolicy::Discouraged_DO_NOT_USE;
            }
        }
        let credential = self
            .authenticator
            .do_registration(Url::parse(ORIGIN).unwrap(), ccr)
            .unwrap();
        self.handles.insert(credential.raw_id.to_vec(), user_handle);
        credential
    }

    /// Answers a discoverable sign-in with the credential registered for `credential_id`.
    pub fn sign_in(
        &mut self,
        mut rcr: RequestChallengeResponse,
        credential_id: &[u8],
    ) -> PublicKeyCredential {
        assert!(rcr.public_key.allow_credentials.is_empty(), "discoverable");
        assert!(rcr.mediation.is_none(), "modal, not conditional");
        rcr.public_key.allow_credentials = vec![AllowCredentials {
            type_: "public-key".to_owned(),
            id: credential_id.to_vec().into(),
            transports: None,
        }];
        let mut assertion = self
            .authenticator
            .do_authentication(Url::parse(ORIGIN).unwrap(), rcr)
            .unwrap();
        assertion.response.user_handle = self.handles.get(credential_id).cloned().map(Into::into);
        assertion
    }
}

// --- Mock Google ---------------------------------------------------------------------------

/// The RSA key the mock signs ID tokens with (and a second one it never publishes).
struct Keys {
    published: PKey<openssl::pkey::Private>,
    unpublished: PKey<openssl::pkey::Private>,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| Keys {
        published: PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap(),
        unpublished: PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap(),
    })
}

/// What the token endpoint returns for one authorization code.
#[derive(Clone)]
pub struct Grant {
    /// The PKCE challenge the code is bound to.
    pub code_challenge: String,
    /// ID token claims.
    pub claims: Value,
    /// Sign with a key missing from the JWKS.
    pub sign_with_unpublished_key: bool,
    /// Sign with HS256 keyed by the client secret (an algorithm confusion attempt).
    pub sign_hs256_with_client_secret: bool,
}

impl Grant {
    /// A normal grant: RS256 with the published key.
    pub fn new(code_challenge: String, claims: Value) -> Self {
        Self {
            code_challenge,
            claims,
            sign_with_unpublished_key: false,
            sign_hs256_with_client_secret: false,
        }
    }
}

#[derive(Clone)]
pub struct MockGoogle {
    pub issuer: String,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

impl MockGoogle {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let mock = Self {
            issuer,
            grants: Arc::default(),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(mock.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        mock
    }

    /// Makes `code` redeemable once, for `grant`.
    pub fn grant(&self, code: &str, grant: Grant) {
        self.grants.lock().unwrap().insert(code.to_owned(), grant);
    }

    /// Valid claims for `subject`, answering the authorization URL `auth_url`.
    pub fn claims(&self, auth_url: &str, subject: &str) -> Value {
        let now = super::session::now_unix();
        json!({
            "iss": self.issuer,
            "aud": CLIENT_ID,
            "sub": subject,
            "nonce": query_param(auth_url, "nonce"),
            "iat": now,
            "exp": now + 3600,
            "email": "shared@example.com",
            "email_verified": true,
        })
    }
}

pub fn query_param(url: &str, name: &str) -> String {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("no {name} in {url}"))
}

async fn discovery(State(mock): State<MockGoogle>) -> Response {
    axum::Json(json!({
        "issuer": mock.issuer,
        "authorization_endpoint": format!("{}/auth", mock.issuer),
        "token_endpoint": format!("{}/token", mock.issuer),
        "jwks_uri": format!("{}/jwks", mock.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
    .into_response()
}

async fn jwks() -> Response {
    let rsa = keys().published.rsa().unwrap();
    axum::Json(json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": "published",
            "n": B64URL.encode(rsa.n().to_vec()),
            "e": B64URL.encode(rsa.e().to_vec()),
        }]
    }))
    .into_response()
}

fn sign_jwt(claims: &Value, key: &PKey<openssl::pkey::Private>, kid: &str) -> String {
    sign_jwt_with(claims, key, kid, "RS256")
}

fn sign_jwt_with(
    claims: &Value,
    key: &PKey<openssl::pkey::Private>,
    kid: &str,
    alg: &str,
) -> String {
    let header = json!({ "alg": alg, "typ": "JWT", "kid": kid });
    let input = format!(
        "{}.{}",
        B64URL.encode(header.to_string()),
        B64URL.encode(claims.to_string())
    );
    let mut signer = Signer::new(MessageDigest::sha256(), key).unwrap();
    signer.update(input.as_bytes()).unwrap();
    format!("{input}.{}", B64URL.encode(signer.sign_to_vec().unwrap()))
}

async fn token(State(mock): State<MockGoogle>, headers: HeaderMap, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let bad = |error: &str| {
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": error })),
        )
            .into_response()
    };

    // client_secret_basic
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&expected)
    {
        return bad("invalid_client");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code")
        || form.get("redirect_uri").map(String::as_str)
            != Some("http://localhost:8080/auth/google/callback")
    {
        return bad("invalid_request");
    }
    let Some(grant) = form
        .get("code")
        .and_then(|code| mock.grants.lock().unwrap().remove(code))
    else {
        return bad("invalid_grant");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    if B64URL.encode(Sha256::digest(verifier.as_bytes())) != grant.code_challenge {
        return bad("invalid_grant");
    }
    let id_token = if grant.sign_hs256_with_client_secret {
        let key = PKey::hmac(CLIENT_SECRET.as_bytes()).unwrap();
        sign_jwt_with(&grant.claims, &key, "published", "HS256")
    } else if grant.sign_with_unpublished_key {
        sign_jwt(&grant.claims, &keys().unpublished, "unpublished")
    } else {
        sign_jwt(&grant.claims, &keys().published, "published")
    };
    axum::Json(json!({
        "access_token": "mock-access-token",
        "token_type": "Bearer",
        "expires_in": 3600,
        "id_token": id_token,
    }))
    .into_response()
}
