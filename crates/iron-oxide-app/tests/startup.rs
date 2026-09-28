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
