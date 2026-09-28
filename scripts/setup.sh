#!/usr/bin/env bash
# Checks the tools this repository needs and installs the missing Rust ones. Run it through
# `make setup` (or `make setup DRY_RUN=1` to only check), which passes the pinned versions read
# from rust-toolchain.toml and Cargo.lock. Idempotent, and never uses sudo: what it cannot install
# itself (rustup, Docker, ...) is listed with how to install it.
set -euo pipefail

: "${RUST_TOOLCHAIN:?}" "${DX_VERSION:?}" "${WASM_TARGET:?}" "${CARGO_BIN:?}"
SQLX_VERSION=${SQLX_VERSION:-}
DRY_RUN=${DRY_RUN:-0}

cd "$(dirname "$0")/.."

missing=0
ok() { printf '  ok        %s\n' "$*"; }
miss() { printf '  MISSING   %s\n' "$*"; missing=$((missing + 1)); }
warn() { printf '  warning   %s\n' "$*"; }
opt() { printf '  optional  %s\n' "$*"; }
act() { printf '  install   %s\n' "$*"; }
# The first two fields of `<tool> --version`'s output, e.g. "dioxus 0.7.10 (abc)" -> "0.7.10".
version_of() { "$@" --version 2>/dev/null | awk '{ print $2; exit }'; }

echo "Rust"
if ! command -v rustup >/dev/null 2>&1; then
  miss "rustup: install it from https://rustup.rs, then run 'make setup' again"
else
  # make puts rustup's cargo first on the PATH, but a shell may not (see README, "Toolchain").
  for other in /opt/homebrew/bin/cargo /usr/local/bin/cargo; do
    if [ -x "$other" ] && [ "$other" != "$CARGO_BIN/cargo" ]; then
      warn "$other (Homebrew's rust?) ignores rust-toolchain.toml: put $CARGO_BIN first on your shell's PATH"
    fi
  done
  installed_toolchain=false
  if rustup toolchain list | grep -q "^${RUST_TOOLCHAIN}-"; then
    installed=$(rustup target list --installed --toolchain "$RUST_TOOLCHAIN" 2>/dev/null || true)
    components=$(rustup component list --installed --toolchain "$RUST_TOOLCHAIN" 2>/dev/null || true)
    if grep -qx "$WASM_TARGET" <<<"$installed" && grep -q '^rustfmt' <<<"$components" &&
      grep -q '^clippy' <<<"$components"; then
      installed_toolchain=true
    fi
  fi
  if $installed_toolchain; then
    ok "Rust $RUST_TOOLCHAIN with rustfmt, clippy and $WASM_TARGET"
  elif [ "$DRY_RUN" = 1 ]; then
    miss "Rust $RUST_TOOLCHAIN with rustfmt, clippy and $WASM_TARGET (rustup toolchain install)"
  else
    act "Rust $RUST_TOOLCHAIN (from rust-toolchain.toml)"
    rustup toolchain install
  fi
fi

echo "Rust tools"
dx_have=$(version_of dx || true)
if [ "$dx_have" = "$DX_VERSION" ]; then
  ok "dx $DX_VERSION"
elif [ "$DRY_RUN" = 1 ]; then
  miss "dx $DX_VERSION (installed: ${dx_have:-none})"
elif command -v cargo-binstall >/dev/null 2>&1; then
  act "dx $DX_VERSION (prebuilt, cargo binstall)"
  cargo binstall -y --force "dioxus-cli@$DX_VERSION"
else
  act "dx $DX_VERSION (cargo install: compiles for several minutes; 'cargo binstall' is faster)"
  cargo install dioxus-cli --version "$DX_VERSION" --locked --force
fi

if [ -z "$SQLX_VERSION" ]; then
  opt "sqlx-cli: skipped, sqlx is not in Cargo.lock"
else
  sqlx_have=$(version_of sqlx || true)
  if [ "$sqlx_have" = "$SQLX_VERSION" ]; then
    ok "sqlx-cli $SQLX_VERSION"
  elif [ "$DRY_RUN" = 1 ]; then
    miss "sqlx-cli $SQLX_VERSION (installed: ${sqlx_have:-none})"
  else
    act "sqlx-cli $SQLX_VERSION"
    cargo install sqlx-cli --version "$SQLX_VERSION" --locked --force \
      --no-default-features --features postgres,rustls
  fi
fi

echo "Docker (local Postgres)"
if [ "$(uname -s)" = Darwin ]; then
  docker_hint="brew install colima docker docker-compose && colima start (or Docker Desktop)"
else
  docker_hint="https://docs.docker.com/engine/install/"
fi
if ! command -v docker >/dev/null 2>&1; then
  miss "docker: $docker_hint"
elif ! compose=$(docker compose version --short 2>/dev/null); then
  miss "docker compose v2: $docker_hint"
else
  # docker-compose.yml uses inline `configs` content, which needs Compose 2.23 or newer.
  if awk -v v="${compose#v}" 'BEGIN { split(v, p, "."); exit !(p[1] > 2 || (p[1] == 2 && p[2] >= 23)) }'; then
    ok "docker compose $compose"
  else
    miss "docker compose 2.23 or newer (found $compose)"
  fi
  if docker info >/dev/null 2>&1; then
    ok "Docker daemon running"
  else
    miss "Docker daemon not running: start Docker Desktop, or 'colima start'"
  fi
fi

echo "Optional"
check_opt() {
  if command -v "$1" >/dev/null 2>&1; then ok "$1 ($2)"; else opt "$1 not installed ($2): $3"; fi
}
check_opt node "service worker tests in 'make test'" "brew install node"
check_opt psql "'make smoke'" "brew install libpq"
check_opt gitleaks "'make secrets'" "brew install gitleaks"
check_opt fly "'make deploy', 'make logs'" "brew install flyctl"
check_opt adb "Android helpers" "Android Studio, then add platform-tools to the PATH"
check_opt tailscale "real iPhone over HTTPS" "brew install --cask tailscale-app"

echo
if [ "$missing" -gt 0 ]; then
  echo "$missing required item(s) missing: see above."
  exit 1
fi
echo "All required tools are installed."
