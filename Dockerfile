# syntax=docker/dockerfile:1

# Production image for the Iron Oxide fullstack app (see docs/operations/deploy.md).
#
#   docker build -t iron-oxide .
#   docker run --rm --init -p 8080:8080 \
#     --add-host=host.docker.internal:host-gateway \
#     -e APP_BASE_URL=http://localhost:8080 \
#     -e DATABASE_URL=postgres://iron_oxide:iron_oxide@host.docker.internal:5433/iron_oxide \
#     iron-oxide
#
# The server needs a reachable Postgres at startup: here the local docker compose database.
#
# Stage 1 builds the release bundle with `dx bundle --web --release` (the same command as the CI
# `dx-bundle` job). Stage 2 is a distroless image holding only the server binary and the client
# assets, running as a non-root user.

# ---------------------------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------------------------
# Same Rust version as rust-toolchain.toml. Debian trixie, to match the runtime's glibc.
FROM rust:1.98.1-slim-trixie@sha256:4cd829461bd5c4d511c32e269da9cb8929223b666519d8004e35fc8d1d771ab7 AS builder

# dx must be the same version as the `dioxus` crates pinned in Cargo.toml (Renovate bumps both in
# one PR), so it is not simply the latest dioxus-cli.
# renovate: datasource=crate depName=dioxus-cli
ARG DX_VERSION=0.7.10
# Set by BuildKit: amd64 on Fly's builders, arm64 on Apple Silicon.
ARG TARGETARCH
# Parallel rustc jobs. Lower it (--build-arg CARGO_BUILD_JOBS=2) on a small Docker VM.
# Unset (the default) means one job per CPU.
ARG CARGO_BUILD_JOBS

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Official prebuilt dx from the Dioxus GitHub release, installed the same way as in CI
# (.github/workflows/ci.yml). The archive is checked against the `.sha256` file published with the
# release, so DX_VERSION is the only input and a Renovate bump needs no hand-edited checksum.
# That catches a corrupt or truncated download; the origin itself is authenticated by TLS to
# github.com. (`cargo install dioxus-cli --locked` would verify every crate against crates.io, but
# compiling dx needs more than 2 GiB of RAM and about ten minutes on every cold builder.)
RUN set -eu; \
    case "${TARGETARCH}" in \
        amd64) arch=x86_64 ;; \
        arm64) arch=aarch64 ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    base="https://github.com/DioxusLabs/dioxus/releases/download/v${DX_VERSION}"; \
    archive="dx-${arch}-unknown-linux-gnu.tar.gz"; \
    checksums="dx-${arch}-unknown-linux-gnu.sha256"; \
    tmp="$(mktemp -d)"; \
    cd "${tmp}"; \
    curl -sSfLO "${base}/${archive}"; \
    curl -sSfLO "${base}/${checksums}"; \
    grep " ${archive}\$" "${checksums}" | sha256sum --check --strict; \
    tar -xzf "${archive}" -C /usr/local/cargo/bin dx; \
    cd /; \
    rm -rf "${tmp}"; \
    dx --version

WORKDIR /app

# Install the toolchain pinned in rust-toolchain.toml (with the wasm32 target) in its own layer.
COPY rust-toolchain.toml ./
RUN rustup toolchain install && rustc --version

COPY . .

# sqlx query macros compile from the committed `.sqlx/` metadata, never from a live database.
ENV SQLX_OFFLINE=true

# Dependency caching uses BuildKit cache mounts rather than cargo-chef: dx builds the server with
# `--features server` and the client for wasm32 with `--features web`, which `cargo chef cook`
# does not reproduce. The cargo registry and `target/` persist between builds on the same builder.
# Do not set RUSTFLAGS here: it would override .cargo/config.toml (wasm needs its cfg flag).
# `target/` is a cache mount, so the bundle is copied out to /out in the same step. The web output
# directory is removed first: dx does not delete old hashed bundles, and the cache mount would
# otherwise ship every `.wasm`/`.js` ever built on this builder. The compile cache is kept.
RUN --mount=type=cache,id=iron-oxide-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=iron-oxide-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=iron-oxide-target,target=/app/target \
    set -eu; \
    if [ -z "${CARGO_BUILD_JOBS:-}" ]; then unset CARGO_BUILD_JOBS; fi; \
    rm -rf target/dx/iron-oxide-app/release/web; \
    dx bundle --web --release -p iron-oxide-app; \
    mkdir -p /out; \
    cp -a target/dx/iron-oxide-app/release/web /out/app; \
    ls -la /out/app /out/app/public

# ---------------------------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------------------------
# glibc, libgcc, CA certificates (TLS to Neon and Google) and a `nonroot` user; no shell.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97 AS runtime

# The server serves the client assets from `public/` next to its binary.
COPY --from=builder --chown=root:root /out/app /app
WORKDIR /app

# `dioxus::serve` binds to IP:PORT. Fly routes to internal_port 8080 (fly.toml).
ENV IP=0.0.0.0 \
    PORT=8080
EXPOSE 8080

USER nonroot:nonroot
ENTRYPOINT ["/app/server"]
