#!/usr/bin/env bash
# Smoke test of the release bundle built by `dx bundle --web --release`: runs its server against a
# Postgres database and checks the page, a server function round-trip, the probes, the wasm client,
# the PWA files, the startup migrations and a graceful shutdown, all over HTTP.
#
# Used by `make smoke` and by CI's `dx bundle + smoke test` job. Run it from the repository root:
#
#   DATABASE_URL=postgres://... scripts/smoke-test.sh <bundle dir>
#
# Environment: DATABASE_URL (required; the server applies the migrations to it), PORT (default
# 8080), SMOKE_LOG (where the server log goes; default: a temporary file, printed on failure).
set -euo pipefail

bundle=${1:?usage: scripts/smoke-test.sh <bundle dir>}
: "${DATABASE_URL:?DATABASE_URL must point at a Postgres database}"
port=${PORT:-8080}
base="http://127.0.0.1:${port}"
log=${SMOKE_LOG:-}
print_log=false
if [ -z "$log" ]; then
  log=$(mktemp)
  print_log=true
fi

if curl -sf -o /dev/null "$base/healthz"; then
  echo "smoke-test: something already answers on port $port; stop it or set PORT." >&2
  exit 1
fi

server_pid=
on_exit() {
  status=$?
  { set +x; } 2>/dev/null
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then kill "$server_pid" || true; fi
  if [ "$status" -ne 0 ] && $print_log; then
    echo "--- server log ---" >&2
    cat "$log" >&2
  fi
  if $print_log; then rm -f "$log"; fi
}
trap on_exit EXIT

ls -la "$bundle"
cd "$bundle"
IP=127.0.0.1 PORT="$port" APP_BASE_URL="$base" ./server >"$log" 2>&1 &
server_pid=$!
for _ in $(seq 1 30); do
  curl -sf "$base/healthz" >/dev/null && break
  sleep 1
done

set -x
test "$(curl -sSf "$base/healthz")" = "ok"
test "$(curl -sSf "$base/readyz")" = "ok"
curl -sSf "$base/" | grep -q "Iron Oxide"
curl -sSf "$base/api/server-time" | grep -Eq '^[0-9]+$'
# The wasm client that hydrates the page is served: the JS loader referenced by the page and every
# .wasm file of the bundle (with the MIME type browsers require).
js=$(curl -sSf "$base/" | grep -oE 'src="[^"]*\.js"' | head -1 | cut -d'"' -f2)
test -n "$js"
curl -sSf -o /dev/null "${base}${js}"
wasm_files=$(find public -name '*.wasm')
test -n "$wasm_files"
for f in $wasm_files; do
  curl -sSf -o /dev/null -w '%{content_type}\n' "$base/${f#public/}" | grep -q '^application/wasm'
done
# PWA: the manifest and head tags are in the SSR page, and the static files are served at the root.
curl -sSf "$base/" | grep -q '<link rel="manifest" href="/manifest.webmanifest"'
curl -sSf "$base/" | grep -q 'navigator.serviceWorker.register(`/sw.js?build='
curl -sSf "$base/manifest.webmanifest" | grep -q '"short_name": "Fe2O3"'
curl -sSf "$base/sw.js?build=ci" | grep -q 'CACHE_VERSION'
for icon in icons/icon-192.png icons/icon-512.png icons/icon-maskable-512.png icons/apple-touch-icon.png favicon.ico; do
  curl -sSf -o /dev/null "$base/$icon"
done
# An unknown hashed asset is a 404, not the SSR page (the service worker must never cache HTML as JS/wasm).
test "$(curl -s -o /dev/null -w '%{http_code}' "$base/assets/missing-dxh0.js")" = "404"
# The startup migrations ran. Not traced: the command line holds the database URL.
{ set +x; } 2>/dev/null
echo "+ the users table exists"
test "$(psql "$DATABASE_URL" -tAc "SELECT to_regclass('users')")" = "users"
set -x
# SIGTERM (Docker's stop signal, and what fly.toml sets) stops it cleanly with status 0.
# SIGINT (Fly's default) is handled the same way; the Postgres tests cover it.
kill -TERM "$server_pid"
wait "$server_pid"
server_pid=
grep -q "shut down" "$log"
{ set +x; } 2>/dev/null
echo "smoke-test: ok"
