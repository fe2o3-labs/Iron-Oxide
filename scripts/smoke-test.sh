#!/usr/bin/env bash
# Smoke test of the release bundle built by `dx bundle --web --release`: runs its server against a
# Postgres database and checks the page, a server function round-trip, the probes, the wasm client,
# the PWA files, the startup migrations, the sign-in endpoints, the Stripe webhook stub and a graceful
# shutdown, all over HTTP.
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
# The public origin the server is configured with (WebAuthn needs a domain, so `localhost`).
origin_url="http://localhost:${port}"
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
headers=
big_body=
on_exit() {
  status=$?
  { set +x; } 2>/dev/null
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then kill "$server_pid" || true; fi
  if [ "$status" -ne 0 ] && $print_log; then
    echo "--- server log ---" >&2
    cat "$log" >&2
  fi
  if $print_log; then rm -f "$log"; fi
  if [ -n "$headers" ]; then rm -f "$headers"; fi
  if [ -n "$big_body" ]; then rm -f "$big_body"; fi
}
trap on_exit EXIT

ls -la "$bundle"
cd "$bundle"
# Sign-in settings as for local development (#5): a random session key per run (never committed,
# never printed) and a placeholder Google client (Google is never called here).
SESSION_KEY="$(openssl rand 64 | openssl base64 -A)"
export SESSION_KEY
IP=127.0.0.1 PORT="$port" APP_BASE_URL="$origin_url" \
  WEBAUTHN_RP_ID=localhost WEBAUTHN_ORIGIN="$origin_url" \
  GOOGLE_CLIENT_ID=placeholder.apps.googleusercontent.com GOOGLE_CLIENT_SECRET=placeholder \
  GOOGLE_REDIRECT_URL="$origin_url/auth/google/callback" \
  ./server >"$log" 2>&1 &
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
echo "+ the sessions table exists"
test "$(psql "$DATABASE_URL" -tAc "SELECT to_regclass('sessions')")" = "sessions"
set -x
# Sign-in (#5): signed out is 401; a cross-site POST is refused; a passkey sign-in starts (sets an
# HttpOnly, SameSite=Lax session cookie, not Secure on local http).
origin="Origin: $origin_url"
headers=$(mktemp)
test "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$origin" -H 'Content-Type: application/json' -d '{}' "$base/api/auth/me")" = "401"
test "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Origin: https://evil.example' -H 'Content-Type: application/json' -d '{}' "$base/api/auth/sign-out")" = "403"
curl -sSf -D "$headers" -X POST -H "$origin" -H 'Content-Type: application/json' -d '{}' \
  "$base/api/auth/passkey/sign-in/begin" | grep -q '"challenge"'
grep -i '^set-cookie: iron_oxide_session=' "$headers" | grep -i 'httponly' | grep -qi 'samesite=lax'
curl -sS "$base/auth/google/callback" | grep -q '"type":"error"'
# Billing (#21): the Stripe webhook stub is reachable cross-site (no Origin, like Stripe) and
# answers 501; a body over its 256 KiB limit is 413.
test "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{}' "$base/webhooks/stripe")" = "501"
big_body=$(mktemp)
head -c 300000 /dev/zero | tr '\0' ' ' > "$big_body"
test "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' --data-binary @"$big_body" "$base/webhooks/stripe")" = "413"
# SIGTERM (Docker's stop signal, and what fly.toml sets) stops it cleanly with status 0.
# SIGINT (Fly's default) is handled the same way; the Postgres tests cover it.
kill -TERM "$server_pid"
wait "$server_pid"
server_pid=
grep -q "shut down" "$log"
{ set +x; } 2>/dev/null
echo "smoke-test: ok"
