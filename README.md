# Iron-Oxide
Zero-cost gains. The only overhead is the barbell

A strength-training PWA written in Rust with [Dioxus](https://dioxuslabs.com) fullstack.

## Layout

| Path | What |
|---|---|
| `crates/iron-oxide-domain` | Pure domain logic. No Dioxus, web-sys or sqlx dependencies; tests run with plain `cargo test -p iron-oxide-domain`. |
| `crates/iron-oxide-app` | The Dioxus fullstack app. The `web` feature builds the browser client (wasm32); the `server` feature builds the axum server (SSR, server functions, `/healthz`). |

## Pinned versions

| Tool | Version | Where |
|---|---|---|
| Rust | 1.98.1 (stable), with the `wasm32-unknown-unknown` target | `rust-toolchain.toml` |
| Dioxus | 0.7.10 (latest stable; 0.8 is still alpha) | `Cargo.toml`, pinned with `=` |
| `dx` (Dioxus CLI) | 0.7.10 | must match the `dioxus` crate version |

## Toolchain

`rust-toolchain.toml` is only honoured when `cargo` is the **rustup** proxy. If Homebrew's `rust`
is installed, its `cargo` ignores the file and uses whatever version Homebrew ships. Check with:

```sh
which cargo        # should be ~/.cargo/bin/cargo, not /opt/homebrew/bin/cargo
cargo --version    # should print 1.98.1
```

If it doesn't, put `~/.cargo/bin` first on your `PATH` (or `brew uninstall rust`). rustup installs
the pinned toolchain and the wasm target on first use.

### wasm `--cfg=web_sys_unstable_apis`

`.cargo/config.toml` passes `--cfg=web_sys_unstable_apis` to wasm32 builds. `web-sys` only exposes
unstable browser APIs behind this cfg, and the rest timer needs one of them: the
[Screen Wake Lock API](https://developer.mozilla.org/docs/Web/API/Screen_Wake_Lock_API), which keeps
the phone screen on during a session. The app refuses to compile for wasm without it, so a build
that bypasses the config (for example, one that sets `RUSTFLAGS`, which overrides it) fails loudly.

## Install the Dioxus CLI

```sh
cargo install dioxus-cli --locked --version 0.7.10
# or a prebuilt binary: cargo binstall dioxus-cli@0.7.10
dx --version   # dioxus 0.7.10
```

## Configuration

The server reads its configuration from environment variables at startup
(`crates/iron-oxide-app/src/server/config.rs`). For local development, copy the template:

```sh
cp .env.example .env
```

`.env` is git-ignored and optional; real environment variables take precedence over it. Never
commit real values: `.env.example` holds placeholders only.

| Variable | Required | What |
|---|---|---|
| `DATABASE_URL` | yes | Postgres connection URL (secret: it embeds the password) |
| `APP_BASE_URL` | yes | Public URL of the app, e.g. `http://localhost:8080` |
| `IP`, `PORT` | no | Bind address, default `127.0.0.1:8080`. `dx serve` sets them itself |
| `RUST_LOG` | no | Log filter, e.g. `info,sqlx=warn` |
| `WEBAUTHN_RP_ID`, `WEBAUTHN_ORIGIN` | with sign-in | Passkeys: our domain and exact origin |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `GOOGLE_REDIRECT_URL` | with sign-in | Sign in with Google |
| `SESSION_KEY` | with sign-in | Session cookie key, ≥ 64 random bytes in base64: `openssl rand 64 \| openssl base64 -A` |

The six sign-in variables are optional until sign-in lands (#5), but all-or-nothing: setting some
of them is an error. Every value is validated at startup. If anything is missing or invalid, the
server prints one line per problem, naming the variable (never its value), and exits with status 1.
Secrets are redacted from `Debug` output and logs.

## Develop

```sh
cp .env.example .env    # once
dx serve --web -p iron-oxide-app
```

This builds the client and the server, and serves the app with hot reload on http://127.0.0.1:8080.
The page has a button that calls the `server_time` server function (`GET /api/server-time`).
The health check is at `GET /healthz`.

Checks run by CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p iron-oxide-app --all-targets --features server -- -D warnings
cargo clippy -p iron-oxide-app --target wasm32-unknown-unknown --features web -- -D warnings
cargo test --workspace
cargo test -p iron-oxide-app --features server
```

`clippy::unwrap_used`, `clippy::expect_used` and `clippy::panic` are denied workspace-wide, but
allowed in tests (`clippy.toml`).

## Release build

```sh
dx bundle --web --release -p iron-oxide-app
```

The output is `target/dx/iron-oxide-app/release/web/`: a `server` binary and the `public/` client
assets next to it. Run it from that directory, with the configuration above in its environment. It
binds to the `IP` and `PORT` environment variables:

```sh
cd target/dx/iron-oxide-app/release/web
IP=0.0.0.0 PORT=8080 DATABASE_URL=... APP_BASE_URL=... ./server
```

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability and for the secrets policy.
Never commit secrets or real `.env` files: CI scans every push and pull request with gitleaks.
