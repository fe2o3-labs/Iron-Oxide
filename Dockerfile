# syntax=docker/dockerfile:1

# Production image for the Iron Oxide fullstack app (see docs/operations/deploy.md).
#
#   docker build -t iron-oxide .
#   docker run --rm --init -p 8080:8080 iron-oxide
#
# Stage 1 builds the release bundle with `dx bundle --web --release` (the same command as the CI
# `dx-bundle` job). Stage 2 is a distroless image holding only the server binary and the client
# assets, running as a non-root user.

# ---------------------------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------------------------
# Same Rust version as rust-toolchain.toml. Debian trixie, to match the runtime's glibc.
FROM rust:1.98.1-slim-trixie@sha256:4cd829461bd5c4d511c32e269da9cb8929223b666519d8004e35fc8d1d771ab7 AS builder

# dx must be the same version as the `dioxus` crates pinned in Cargo.toml (and DX_VERSION in
# .github/workflows/ci.yml), so it is not simply the latest dioxus-cli. On a bump, update both
# checksums from the release's `.sha256` files: a stale checksum fails the build.
# renovate: datasource=crate depName=dioxus-cli
ARG DX_VERSION=0.7.10
ARG DX_SHA256_X86_64=4363e4ed2a3f1eb7f4d38d2d59aed59ce43271c44c16b425e92c89a64761fbe7
ARG DX_SHA256_AARCH64=8f1a17d3218700ffbe15e6540d936a178b2556fc801121a31082e3ba4ab9ef55
# Set by BuildKit: amd64 on Fly's builders, arm64 on Apple Silicon.
ARG TARGETARCH
# Parallel rustc jobs. Lower it (--build-arg CARGO_BUILD_JOBS=2) on a small Docker VM.
# Unset (the default) means one job per CPU.
ARG CARGO_BUILD_JOBS

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Prebuilt dx from the Dioxus GitHub release, checksum-verified.
RUN set -eu; \
    case "${TARGETARCH}" in \
        amd64) arch=x86_64; sha="${DX_SHA256_X86_64}" ;; \
        arm64) arch=aarch64; sha="${DX_SHA256_AARCH64}" ;; \
        *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    archive="dx-${arch}-unknown-linux-gnu.tar.gz"; \
    curl -sSfL -o "/tmp/${archive}" \
        "https://github.com/DioxusLabs/dioxus/releases/download/v${DX_VERSION}/${archive}"; \
    echo "${sha}  /tmp/${archive}" | sha256sum --check --strict; \
    tar -xzf "/tmp/${archive}" -C /usr/local/cargo/bin dx; \
    rm "/tmp/${archive}"; \
    dx --version

WORKDIR /app

# Install the toolchain pinned in rust-toolchain.toml (with the wasm32 target) in its own layer.
COPY rust-toolchain.toml ./
RUN rustup toolchain install && rustc --version

COPY . .

# Dependency caching uses BuildKit cache mounts rather than cargo-chef: dx builds the server with
# `--features server` and the client for wasm32 with `--features web`, which `cargo chef cook`
# does not reproduce. The cargo registry and `target/` persist between builds on the same builder.
# Do not set RUSTFLAGS here: it would override .cargo/config.toml (wasm needs its cfg flag).
# `target/` is a cache mount, so the bundle is copied out to /out in the same step.
RUN --mount=type=cache,id=iron-oxide-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=iron-oxide-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=iron-oxide-target,target=/app/target \
    set -eu; \
    if [ -z "${CARGO_BUILD_JOBS:-}" ]; then unset CARGO_BUILD_JOBS; fi; \
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
