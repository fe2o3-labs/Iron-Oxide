# Iron-Oxide
Zero-cost gains. The only overhead is the barbell

A strength-training PWA written in Rust with [Dioxus](https://dioxuslabs.com) fullstack.

## Layout

| Path | What |
|---|---|
| `crates/iron-oxide-domain` | Pure domain logic. No Dioxus, web-sys or sqlx dependencies; tests run with plain `cargo test -p iron-oxide-domain`. |
| `crates/iron-oxide-app` | The Dioxus fullstack app. The `web` feature builds the browser client (wasm32); the `server` feature builds the axum server (SSR, server functions, `/healthz`). |

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

## Develop

```sh
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
assets next to it. Run it from that directory. It binds to the `IP` and `PORT` environment
variables:

```sh
cd target/dx/iron-oxide-app/release/web
IP=0.0.0.0 PORT=8080 ./server
```

Docker image and Fly.io deployment: see [docs/operations/deploy.md](docs/operations/deploy.md).

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability and for the secrets policy.
Never commit secrets or real `.env` files: CI scans every push and pull request with gitleaks.
To mark a test fixture that gitleaks flags as a false positive, see "Test fixtures that look like
secrets" in SECURITY.md.

## License

Iron Oxide is licensed under the [GNU Affero General Public License v3.0 only](LICENSE)
(`AGPL-3.0-only`). If you run a modified version as a network service, you must offer its users
the corresponding source code.
