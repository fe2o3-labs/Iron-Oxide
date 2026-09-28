//! Starts the real server binary with a bad environment and checks that it fails fast.
#![cfg(feature = "server")]
#![allow(clippy::unwrap_used, reason = "test helpers may unwrap")]

use std::process::{Command, Output};

/// Runs the server binary with only the given variables set, from a directory with no `.env`.
fn run_server(vars: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_iron-oxide-app"))
        .env_clear()
        .envs(vars.iter().copied())
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap()
}

#[test]
fn missing_database_url_exits_non_zero_naming_the_variable() {
    let output = run_server(&[("APP_BASE_URL", "http://localhost:8080")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("DATABASE_URL is not set"), "{stderr}");
    assert!(!stderr.contains("APP_BASE_URL"), "{stderr}");
}

#[test]
fn invalid_values_are_reported_without_echoing_them() {
    let output = run_server(&[
        ("DATABASE_URL", "mysql://user:hunter2@localhost/db"),
        ("APP_BASE_URL", "http://localhost:8080"),
        ("PORT", "99999"),
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("DATABASE_URL is invalid"), "{stderr}");
    assert!(stderr.contains("PORT is invalid"), "{stderr}");
    assert!(!stderr.contains("hunter2"), "{stderr}");
}

/// Sends `GET path` over plain HTTP/1.1 and returns the status line and body.
fn http_get(port: u16, path: &str) -> std::io::Result<(String, String)> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
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

#[cfg(unix)]
#[test]
#[ignore = "needs Postgres"]
fn serves_probes_and_shuts_down_gracefully_on_sigterm() {
    use std::time::{Duration, Instant};

    let database_url = std::env::var("DATABASE_URL").unwrap();
    // Dioxus serves static assets from this directory; the test needs none.
    let public = std::env::temp_dir().join(format!("iron-oxide-public-{}", std::process::id()));
    std::fs::create_dir_all(&public).unwrap();
    let port = free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_iron-oxide-app"))
        .env_clear()
        .env("DATABASE_URL", &database_url)
        .env("APP_BASE_URL", "http://localhost:8080")
        .env("PORT", port.to_string())
        .env("DIOXUS_PUBLIC_PATH", &public)
        .env("RUST_LOG", "info")
        .current_dir(std::env::temp_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    let healthz = loop {
        match http_get(port, "/healthz") {
            Ok(response) => break response,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => {
                let _ = child.kill();
                panic!("server did not come up: {error}");
            }
        }
    };
    assert!(healthz.0.contains("200"), "{healthz:?}");
    assert_eq!(healthz.1, "ok");
    let readyz = http_get(port, "/readyz").unwrap();
    assert!(readyz.0.contains("200"), "{readyz:?}");
    assert_eq!(readyz.1, "ok");

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&public);
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{:?}\n{logs}", output.status);
    assert!(logs.contains("shutdown signal received"), "{logs}");
    assert!(logs.contains("shut down"), "{logs}");
    assert!(
        http_get(port, "/healthz").is_err(),
        "still listening after shutdown"
    );
}
