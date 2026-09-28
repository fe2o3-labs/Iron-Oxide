//! Runs the real server binary: fail-fast configuration, probes, shutdown, and secrets in logs.
#![cfg(feature = "server")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers may unwrap"
)]

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

/// A fresh, empty directory for one test (no `.env` in it).
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("iron-oxide-startup-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the server binary to completion with only the given variables set, from `dir`.
fn run_server_in(dir: &PathBuf, vars: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_iron-oxide-app"))
        .env_clear()
        .envs(vars.iter().copied())
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run_server(name: &str, vars: &[(&str, &str)]) -> Output {
    run_server_in(&temp_dir(name), vars)
}

fn all_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn missing_database_url_exits_non_zero_naming_the_variable() {
    let output = run_server("missing", &[("APP_BASE_URL", "http://localhost:8080")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("DATABASE_URL is not set"), "{stderr}");
    assert!(!stderr.contains("APP_BASE_URL"), "{stderr}");
}

#[test]
fn invalid_values_are_reported_without_echoing_them() {
    let output = run_server(
        "invalid",
        &[
            ("DATABASE_URL", "mysql://user:SECRETxyz@localhost/db"),
            ("APP_BASE_URL", "http://localhost:8080"),
            ("PORT", "99999"),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("DATABASE_URL is invalid"), "{stderr}");
    assert!(stderr.contains("PORT is invalid"), "{stderr}");
    assert!(!stderr.contains("SECRETxyz"), "{stderr}");
}

#[test]
fn a_split_database_password_is_rejected_without_leaking_it() {
    for url in [
        "postgres://leakprobe:/SECRETxyz@127.0.0.1:1/leakdb",
        "postgres://leakprobe:2024/SECRETxyz@127.0.0.1:1/leakdb",
        "postgres://iron_oxide:12?host=SECRETxyz@127.0.0.1:1/iron_oxide",
        "postgres://iron_oxide:?dbname=SECRETxyz@127.0.0.1:1/iron_oxide",
        "postgres://iron_oxide:12?channel_binding=SECRETxyz@127.0.0.1:1/iron_oxide",
        "postgres://iron_oxide:12/iron_oxide?host=SECRETxyz@127.0.0.1:1/x",
    ] {
        let output = run_server(
            "slash",
            &[
                ("DATABASE_URL", url),
                ("APP_BASE_URL", "http://localhost:8080"),
                ("RUST_LOG", "trace"),
            ],
        );
        let logs = all_output(&output);
        assert_eq!(output.status.code(), Some(1), "{logs}");
        assert!(logs.contains("percent-encode"), "{logs}");
        assert!(!logs.contains("SECRETxyz"), "{logs}");
    }
}

#[test]
fn a_bad_env_file_is_reported_by_line_without_its_content() {
    let dir = temp_dir("dotenv");
    std::fs::write(
        dir.join(".env"),
        "APP_BASE_URL=http://localhost:8080\nDATABASE_URL=postgres://u:SECRETxyz@127.0.0.1:1/d x\n",
    )
    .unwrap();
    let output = run_server_in(&dir, &[]);
    let logs = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{logs}");
    assert!(logs.contains(".env has a syntax error on line 2"), "{logs}");
    assert!(!logs.contains("SECRETxyz"), "{logs}");
}

// --- Against Postgres (`DATABASE_URL`, see README): `cargo test ... -- --ignored`. ---

/// Sends `GET path` over plain HTTP/1.1 and returns the status line and body.
fn http_get(port: u16, path: &str) -> std::io::Result<(String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response.lines().next().unwrap_or_default().to_owned();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Sign-in settings for local development (#5). The session key is 64 zero bytes: test-only.
const SIGN_IN_VARS: [(&str, &str); 6] = [
    ("WEBAUTHN_RP_ID", "localhost"),
    ("WEBAUTHN_ORIGIN", "http://localhost:8080"),
    ("GOOGLE_CLIENT_ID", "test-client.apps.googleusercontent.com"),
    ("GOOGLE_CLIENT_SECRET", "test-client-secret"),
    (
        "GOOGLE_REDIRECT_URL",
        "http://localhost:8080/auth/google/callback",
    ),
    (
        "SESSION_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
    ),
];

/// A server process started against the test database, ready to serve.
struct Server {
    child: Option<Child>,
    port: u16,
    public: PathBuf,
}

impl Server {
    /// Starts the binary with `DATABASE_URL` (or `database_url`) and waits for `/healthz`.
    fn start(name: &str, database_url: Option<&str>, extra: &[(&str, &str)]) -> Self {
        let database_url = database_url
            .map(str::to_owned)
            .unwrap_or_else(|| std::env::var("DATABASE_URL").expect("DATABASE_URL"));
        // Dioxus serves static assets from this directory; the tests need none.
        let public = temp_dir(name);
        let port = free_port();
        let child = Command::new(env!("CARGO_BIN_EXE_iron-oxide-app"))
            .env_clear()
            .env("DATABASE_URL", database_url)
            .env("APP_BASE_URL", "http://localhost:8080")
            .envs(SIGN_IN_VARS.iter().copied())
            .env("PORT", port.to_string())
            .env("DIOXUS_PUBLIC_PATH", &public)
            .env("RUST_LOG", "info")
            .envs(extra.iter().copied())
            .current_dir(&public)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut server = Self {
            child: Some(child),
            port,
            public,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match http_get(port, "/healthz") {
                Ok(_) => return server,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(200))
                }
                Err(error) => {
                    let output = server.wait();
                    panic!("server did not come up: {error}\n{}", all_output(&output));
                }
            }
        }
    }

    fn signal(&self, signal: &str) {
        let pid = self.child.as_ref().unwrap().id().to_string();
        assert!(
            Command::new("kill")
                .args([signal, &pid])
                .status()
                .unwrap()
                .success()
        );
    }

    /// Waits for the process to exit (killing it after 60 s) and returns its output.
    fn wait(&mut self) -> Output {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        child.wait_with_output().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.public);
    }
}

/// Opens a connection and sends a request line and one header, but never finishes the request.
fn partial_request(port: u16) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: slow\r\n")
        .unwrap();
    stream
}

#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn serves_probes_and_shuts_down_gracefully_on_sigterm() {
    let mut server = Server::start("sigterm", None, &[]);
    let healthz = http_get(server.port, "/healthz").unwrap();
    assert!(healthz.0.contains("200"), "{healthz:?}");
    assert_eq!(healthz.1, "ok");
    let readyz = http_get(server.port, "/readyz").unwrap();
    assert!(readyz.0.contains("200"), "{readyz:?}");
    assert_eq!(readyz.1, "ok");

    server.signal("-TERM");
    let output = server.wait();
    let logs = all_output(&output);
    assert!(output.status.success(), "{:?}\n{logs}", output.status);
    assert!(logs.contains("signal=\"SIGTERM\""), "{logs}");
    assert!(logs.contains("shut down"), "{logs}");
    assert!(
        http_get(server.port, "/healthz").is_err(),
        "still listening after shutdown"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn sigint_also_shuts_down_gracefully() {
    let mut server = Server::start("sigint", None, &[]);
    server.signal("-INT");
    let output = server.wait();
    let logs = all_output(&output);
    assert!(output.status.success(), "{:?}\n{logs}", output.status);
    assert!(logs.contains("signal=\"SIGINT\""), "{logs}");
}

#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn a_stuck_client_cannot_hold_shutdown_past_the_grace_period() {
    let mut server = Server::start("slowloris", None, &[("SHUTDOWN_GRACE_SECS", "2")]);
    let _stuck = partial_request(server.port);
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    server.signal("-TERM");
    let output = server.wait();
    let elapsed = started.elapsed();
    let logs = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{logs}");
    assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}\n{logs}");
    assert!(elapsed < Duration::from_secs(8), "{elapsed:?}\n{logs}");
    assert!(logs.contains("after the grace period"), "{logs}");
    assert!(logs.contains("shut down"), "{logs}");
}

#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn a_second_signal_stops_the_server_at_once() {
    let mut server = Server::start("second-signal", None, &[("SHUTDOWN_GRACE_SECS", "120")]);
    let _stuck = partial_request(server.port);
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    server.signal("-TERM");
    std::thread::sleep(Duration::from_millis(500));
    server.signal("-INT");
    let output = server.wait();
    let logs = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{logs}");
    assert!(started.elapsed() < Duration::from_secs(8), "{logs}");
    assert!(logs.contains("second shutdown signal"), "{logs}");
}

/// Starts the server as a role whose password is full of URL-breaking characters
/// (percent-encoded, as it must be), with trace logging, and checks the password never shows up.
#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn a_nasty_database_password_never_reaches_the_logs() {
    use sqlx::{Connection, PgConnection};

    let admin_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let role = format!("iron_oxide_nasty_{}", std::process::id());
    let password = "/a@b:c#d?e%f SECRETxyz";
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let admin = |sql: String| {
        runtime.block_on(async {
            let mut conn = PgConnection::connect(&admin_url).await.unwrap();
            sqlx::raw_sql(&sql).execute(&mut conn).await.unwrap();
        })
    };
    let owner = runtime.block_on(async {
        let mut conn = PgConnection::connect(&admin_url).await.unwrap();
        sqlx::query_scalar::<_, String>("SELECT current_user::text")
            .fetch_one(&mut conn)
            .await
            .unwrap()
    });
    // Migrate as the owner first, so the new role never owns anything and drops cleanly.
    runtime.block_on(async {
        let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let migrator = sqlx::migrate::Migrator::new(migrations).await.unwrap();
        let mut conn = PgConnection::connect(&admin_url).await.unwrap();
        migrator.run(&mut conn).await.unwrap();
    });
    admin(format!(
        "DROP ROLE IF EXISTS {role}; CREATE ROLE {role} LOGIN PASSWORD '{password}' IN ROLE {owner}"
    ));

    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some(password)).unwrap();
    let encoded = url.to_string();

    let mut server = Server::start("nasty-password", Some(&encoded), &[("RUST_LOG", "trace")]);
    let readyz = http_get(server.port, "/readyz").unwrap();
    server.signal("-TERM");
    let output = server.wait();
    admin(format!("DROP OWNED BY {role}; DROP ROLE IF EXISTS {role}"));

    let logs = all_output(&output);
    assert!(readyz.0.contains("200"), "{readyz:?}\n{logs}");
    assert!(output.status.success(), "{logs}");
    assert!(logs.contains("connected to Postgres"), "{logs}");
    assert!(!logs.contains("SECRETxyz"), "{logs}");
    let encoded_password = url.password().unwrap_or_default();
    assert!(!logs.contains(encoded_password), "{logs}");
    // The sign-in secrets never reach the logs either, even at trace level.
    assert!(!logs.contains("test-client-secret"), "{logs}");
    assert!(!logs.contains("AAAAAAAAAAAAAAAA"), "{logs}");
}
