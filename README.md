# Iron-Oxide
Zero-cost gains. The only overhead is the barbell

A strength-training PWA written in Rust with [Dioxus](https://dioxuslabs.com) fullstack.

## Layout

| Path | What |
|---|---|
| `crates/iron-oxide-domain` | Pure domain logic. No Dioxus, web-sys or sqlx dependencies; tests run with plain `cargo test -p iron-oxide-domain`. |
| `crates/iron-oxide-app` | The Dioxus fullstack app. The `web` feature builds the browser client (wasm32); the `server` feature builds the axum server (SSR, server functions, `/healthz`, `/readyz`). |
| `crates/iron-oxide-app/migrations` | SQL migrations, embedded in the server and applied at startup. |
| `.sqlx/` | Offline query metadata for the sqlx macros (see "Database"). |
| `docker-compose.yml` | Local Postgres for development and tests. |

## Pinned versions

Everything is pinned to its latest stable release and kept current by Renovate (`renovate.json`).

| Tool | Where it is pinned |
|---|---|
| Rust (stable) + the `wasm32-unknown-unknown` target | `rust-toolchain.toml` |
| Dioxus | `dioxus` in `Cargo.toml`, pinned exactly with `=` |
| `dx` (Dioxus CLI) | this README, `.github/workflows/ci.yml` (and the Dockerfile) |

The `dx` version must always equal the `dioxus` crate version. Renovate bumps them together in one
PR, through the `# renovate: datasource=crate depName=dioxus-cli` markers.

## Toolchain

`rust-toolchain.toml` is only honoured when `cargo` is the **rustup** proxy. If Homebrew's `rust`
is installed, its `cargo` ignores the file and uses whatever version Homebrew ships. Check with:

```sh
which cargo        # should be ~/.cargo/bin/cargo, not /opt/homebrew/bin/cargo
cargo --version    # should print the version pinned in rust-toolchain.toml
```

If it doesn't, put `~/.cargo/bin` first on your `PATH` (or `brew uninstall rust`). rustup installs
the pinned toolchain and the wasm target on first use.

### wasm `--cfg=web_sys_unstable_apis`

`.cargo/config.toml` passes `--cfg=web_sys_unstable_apis` to wasm32 builds. `web-sys` only exposes
unstable browser APIs behind this cfg, and the rest timer needs one of them: the
[Screen Wake Lock API](https://developer.mozilla.org/docs/Web/API/Screen_Wake_Lock_API), which keeps
the phone screen on during a session. The app refuses to compile for wasm without it, so a build
that bypasses the config fails loudly.

> **`RUSTFLAGS` replaces `.cargo/config.toml`'s `rustflags`; it does not add to them.** If you set
> `RUSTFLAGS` (or `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS`) for a build that includes the
> wasm client, for example in a Dockerfile or a CI step, it must also contain
> `--cfg=web_sys_unstable_apis`.

## Install the Dioxus CLI

```sh
# renovate: datasource=crate depName=dioxus-cli
cargo install dioxus-cli --locked --version 0.7.10
dx --version   # must match the `dioxus` version in Cargo.toml
```

`cargo binstall dioxus-cli@<same version>` installs the official prebuilt binary instead, which is much faster.

## Configuration

The server reads its configuration from environment variables at startup
(`crates/iron-oxide-app/src/server/config.rs`). For local development, copy the template:

```sh
cp .env.example .env
```

`.env` is git-ignored and optional, and only read from the working directory (not its parents);
real environment variables take precedence over it. A malformed `.env` stops the server with the
line number only, never the line's content. Never commit real values: `.env.example` holds
placeholders only.

In `DATABASE_URL`, percent-encode special characters in the user name and password (`/` as
`%2F`, `@` as `%40`, `#` as `%23`, `?` as `%3F`, `%` as `%25`). An unencoded one splits the URL in
the wrong place, so the server rejects such URLs rather than risk logging part of the password.
Logs only ever show `host:port/database`.

Query parameters are limited to the ones sqlx supports: `sslmode` and the TLS file options,
`statement-cache-capacity`, `dbname`, `user`, `password`, `application_name` and `options`, plus
the Neon ones below. Anything else, including `host`, `hostaddr` and `port` (the URL names the only
host), stops the server with "unsupported parameter".

`WEBAUTHN_ORIGIN` and `GOOGLE_REDIRECT_URL` must use `https://`, except on `localhost`,
`127.0.0.1` or `[::1]`.

| Variable | Required | What |
|---|---|---|
| `DATABASE_URL` | yes | Postgres connection URL (secret: it embeds the password) |
| `APP_BASE_URL` | yes | Public URL of the app, e.g. `http://localhost:8080` |
| `IP`, `PORT` | no | Bind address, default `127.0.0.1:8080`. `dx serve` sets them itself |
| `RUST_LOG` | no | Log filter, e.g. `info,sqlx=warn` |
| `SHUTDOWN_GRACE_SECS` | no | Time in-flight requests get after a shutdown signal, 1 to 300, default 20. Keep it below the platform's kill timeout |
| `WEBAUTHN_RP_ID`, `WEBAUTHN_ORIGIN` | with sign-in | Passkeys: our domain and exact origin |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `GOOGLE_REDIRECT_URL` | with sign-in | Sign in with Google |
| `SESSION_KEY` | with sign-in | Session cookie key, ≥ 64 random bytes in base64: `openssl rand 64 \| openssl base64 -A` |

The six sign-in variables are optional until sign-in lands (#5), but all-or-nothing: setting some
of them is an error. Every value is validated at startup. If anything is missing or invalid, the
server prints one line per problem, naming the variable (never its value), and exits with status 1.
Secrets are redacted from `Debug` output and logs.

## Database

Postgres 18 (the same major version as the Neon project) runs locally with Docker Compose
(Compose 2.23 or newer):

```sh
docker compose up -d --wait   # dev DB `iron_oxide` and test DB `iron_oxide_test`, on localhost:5433
docker compose down           # stop (add -v to delete the data)
```

It listens on port 5433 so it does not clash with a local Postgres on 5432; set
`IRON_OXIDE_PG_PORT` to use another port (and change `DATABASE_URL` to match).

### Migrations

Migrations live in `crates/iron-oxide-app/migrations/` and are embedded in the server binary
(`sqlx::migrate!()`). The server applies any pending ones at startup, before serving requests;
there is no separate migration step to run on deploy.

The CLI is only needed to add migrations or refresh the query data below. Install the version that
matches the `sqlx` crate:

```sh
# renovate: datasource=crate depName=sqlx-cli
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features postgres,rustls
sqlx migrate add <name> --source crates/iron-oxide-app/migrations
sqlx migrate run --source crates/iron-oxide-app/migrations   # optional: the server does it too
```

### Offline query data (`.sqlx/`)

`sqlx::query!` macros check queries against a real database at compile time. So that CI, the
Docker build and anyone without a running database can still compile, the query metadata is
committed in `.sqlx/`, and CI builds with `SQLX_OFFLINE=true`. After adding or changing a query,
with the compose database up and migrated:

```sh
cargo sqlx prepare --workspace -- --all-targets --features iron-oxide-app/server
git add .sqlx
```

CI fails if `.sqlx/` is missing a query or holds a stale one.

Whenever `DATABASE_URL` is set (including from `.env`), the macros check queries against that live
database instead of `.sqlx/`, so its schema must be migrated. On a fresh database, either run
`sqlx migrate run --source crates/iron-oxide-app/migrations` before the first build, or build with
`SQLX_OFFLINE=true`, which compiles from `.sqlx/` without a database.

### Tests that need Postgres

They are marked `#[ignore = "needs Postgres"]`, so plain `cargo test` skips them. Run them against
the compose test database; each `#[sqlx::test]` creates, and then drops, its own database:

```sh
DATABASE_URL=postgres://iron_oxide:iron_oxide@localhost:5433/iron_oxide_test \
  cargo test -p iron-oxide-app --features server -- --ignored
```

### Neon (production)

- Use the **direct** endpoint for `DATABASE_URL`: the host **without** `-pooler` (decision #39).
  The startup migrations hold a session-level advisory lock, which Neon's transaction-mode pooler
  (PgBouncer) cannot keep across transactions. The app's own pool is small (5 connections), so it
  does not need Neon's pooler.
- Keep `?sslmode=require` (or `verify-full`). Neon's connection strings can be pasted as they are.
  - `options=endpoint%3D...` and `application_name` are passed to sqlx.
  - `channel_binding`, `connect_timeout` and `sslnegotiation` are accepted but removed before
    the URL reaches sqlx, which does not support them. TLS still applies through `sslmode`, and
    the app bounds each connection attempt itself.
- The Neon project must run the same Postgres major version as `docker-compose.yml` and CI (18).
- Pool settings follow Neon's advice: at most 5 connections, none kept while idle, idle
  connections closed after 2 minutes, every connection recycled after 5 minutes, and the first
  connection retried with backoff (up to 6 attempts) while a suspended compute wakes up.
- Point the platform's frequent health check at `/healthz`, which never queries the database.
  `/readyz` runs `SELECT 1`, so polling it keeps the Neon compute awake and uses up the Free
  plan's compute hours.

## Develop

```sh
docker compose up -d --wait   # once per session
cp .env.example .env          # once
dx serve --web -p iron-oxide-app
```

This builds the client and the server, and serves the app with hot reload on http://127.0.0.1:8080.
The page has a button that calls the `server_time` server function (`GET /api/server-time`).
Probes:

- `GET /healthz`: liveness, always `200 ok` while the process serves HTTP. It never touches the
  database, so the platform can poll it often without keeping a scale-to-zero Neon compute awake.
- `GET /readyz`: readiness, `200 ok` when Postgres answers `SELECT 1` within 2 seconds, `503`
  otherwise (the cause is logged, not returned).

On SIGINT (Ctrl-C, and Fly's default kill signal) or SIGTERM (Docker's), the server stops accepting
connections and lets in-flight requests finish, then closes the database pool and exits with
status 0. The drain is bounded by `SHUTDOWN_GRACE_SECS` (default 20 s, below the 30 s
`kill_timeout` in `fly.toml`): past it, remaining connections, such as a client that never
finishes its request, are dropped and the exit status is 1. A second signal stops the server at
once.

### Shared server state in server functions

At startup the server loads the config, connects to Postgres and applies the migrations, then
attaches an `AppState` (config and connection pool) to every request as an axum `Extension`.
A server function takes it as an extra, server-only argument after the route:

```rust
#[cfg(feature = "server")]
use {crate::server::AppState, dioxus::server::axum::Extension};

#[get("/api/me", state: Extension<AppState>)]
pub async fn me() -> Result<Profile, ServerFnError> {
    let pool: &sqlx::PgPool = &state.db;
    // ... query with `pool`, scoped to the signed-in user.
}
```

`State<AppState>` does not work there, because Dioxus uses the axum router state for itself.

Checks run by CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p iron-oxide-app --all-targets --features server -- -D warnings
cargo clippy -p iron-oxide-app --target wasm32-unknown-unknown --features web -- -D warnings
cargo test --workspace
cargo test -p iron-oxide-app --features server
SQLX_OFFLINE=true cargo check -p iron-oxide-app --all-targets --features server
# with Postgres (see "Database"):
cargo sqlx prepare --workspace --check -- --all-targets --features iron-oxide-app/server
cargo test -p iron-oxide-app --features server -- --ignored
```

`clippy::unwrap_used`, `clippy::expect_used` and `clippy::panic` are denied workspace-wide, but
allowed in tests (`clippy.toml`).

## PWA

The app is an installable Progressive Web App.

| Path (in `crates/iron-oxide-app`) | What |
|---|---|
| `public/manifest.webmanifest` | Web app manifest, served at `/manifest.webmanifest` |
| `public/sw.js` | Service worker, served at `/sw.js` so that its scope is the whole app |
| `public/icons/`, `public/favicon.ico` | Generated icons (192, 512, maskable 512, apple-touch 180, favicon) |
| `icons/` | SVG sources for the icons, plus `render.sh` to regenerate them (needs `rsvg-convert` and ImageMagick) |
| `assets/tokens.css` | Colour tokens (`--io-*`), documented in [docs/palette.md](docs/palette.md) |
| `src/pwa.rs` | Manifest link, icons and iOS meta tags in `<head>`, plus service worker registration |

`dx` copies `public/` unchanged to the root of the site. The service worker is only registered in
release builds, because `dx serve` rebuilds constantly and serves an unhashed `/wasm/` folder. To test
the PWA locally, run the release bundle (see below) and open http://127.0.0.1:8080. Chrome and
Safari treat `localhost`/`127.0.0.1` as a secure context, so no HTTPS is needed.

The page registers the worker as `/sw.js?build=<id>`, where the id is derived from the hashed asset
URLs. Every deploy that changes the wasm, JS or CSS therefore installs a fresh worker with its own
cache, and the old cache is deleted. Bump `CACHE_VERSION` in `sw.js` when only `sw.js` or an
unhashed file in `public/` (icons, manifest) changes. What the service worker caches:

- Hashed files under `/assets/` (wasm, JS, CSS): cache-first. They never change for a given URL.
- The icons and the manifest are precached at install, and so is an anonymous render of `/`,
  fetched without cookies.
- Page navigations: network-first. When offline, the cached shell is served instead. Navigation
  responses are never cached.
- Anything that is not a same-origin `GET`, and everything under `/api/` or `/auth/`: network-only,
  never cached.
- Only complete, direct responses with a non-HTML `Content-Type` are cached (status 200, not
  redirected; a missing `Content-Type` is not cached). `Range` requests go straight to the network.
  Unit tests: `node --test crates/iron-oxide-app/tests/sw/is_cacheable.test.mjs`.

The server answers `404` (with `Cache-Control: no-store`) for unknown `/assets/…` paths
(`src/pwa/missing_assets.rs`) instead of the SSR page that Dioxus serves for every other unknown path.

To try the app on the iOS Simulator, the Android Emulator or a real phone, see [docs/dev/mobile-testing.md](docs/dev/mobile-testing.md).

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
To mark a test fixture that gitleaks flags as a false positive, see "Test fixtures that look like
secrets" in SECURITY.md.

## License

Iron Oxide is licensed under the [GNU Affero General Public License v3.0 only](LICENSE)
(`AGPL-3.0-only`). If you run a modified version as a network service, you must offer its users
the corresponding source code.
