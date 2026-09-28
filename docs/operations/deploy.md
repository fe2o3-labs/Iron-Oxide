# Deploying to Fly.io

Production runs on [Fly.io](https://fly.io) in Frankfurt (`fra`), next to the Neon database
(AWS Frankfurt). Pushes to `main` deploy automatically through `.github/workflows/deploy.yml`.

| File | What |
|---|---|
| `Dockerfile` | Multi-stage build: `dx bundle --web --release`, then a distroless runtime (non-root, server binary + `public/` assets only). |
| `.dockerignore` | Keeps `target/`, `.env*`, keys and `.git` out of the build context. |
| `fly.toml` | App config: region, port, HTTPS, health check, VM size. No secrets. |
| `.github/workflows/deploy.yml` | Pull requests: build and smoke test the image. Push to `main` / manual run: `flyctl deploy`. |

Secrets never go in the repo, in `fly.toml` or in the image: they are Fly secrets, exposed to the
app as environment variables.

## Build and run the image locally

The server connects to Postgres and runs the migrations before it listens, so start the local
docker compose database first (`docker compose up -d`, see the README), then:

```sh
docker build -t iron-oxide .
docker run --rm --init -p 8080:8080 \
  --add-host=host.docker.internal:host-gateway \
  -e APP_BASE_URL=http://localhost:8080 \
  -e DATABASE_URL=postgres://iron_oxide:iron_oxide@host.docker.internal:5433/iron_oxide \
  -e WEBAUTHN_RP_ID=localhost -e WEBAUTHN_ORIGIN=http://localhost:8080 \
  -e GOOGLE_CLIENT_ID=placeholder.apps.googleusercontent.com -e GOOGLE_CLIENT_SECRET=placeholder \
  -e GOOGLE_REDIRECT_URL=http://localhost:8080/auth/google/callback \
  -e SESSION_KEY="$(openssl rand 64 | openssl base64 -A)" \
  iron-oxide
curl http://127.0.0.1:8080/healthz   # ok
curl http://127.0.0.1:8080/readyz    # ok: the database answers
open http://localhost:8080/
```

Inside the container `localhost` is the container itself; `host.docker.internal` reaches the
compose database published on the host. These are the local development credentials from
`.env.example`, not secrets.

On a small Docker VM (for example Colima's default 2 CPU / 2 GiB), limit parallel compilation
so the linker isn't killed for lack of memory: `docker build --build-arg CARGO_BUILD_JOBS=2 ...`.
The image builds for the host architecture; Fly builds it for amd64 on its own builders.

## First-time setup (maintainer)

Done once, in this order. Commands assume the app name `iron-oxide`; if it is taken, pick
another name and change `app` in `fly.toml` to match.

### 1. Install flyctl and log in

```sh
brew install flyctl        # or: curl -L https://fly.io/install.sh | sh
fly auth login
```

### 2. Create the app

```sh
fly apps create iron-oxide
```

This creates an empty app; nothing runs until the first deploy. Use `fly apps create` rather than
`fly launch`, which would rewrite `fly.toml`.

### 3. Set the secrets

The variables are described in `.env.example` (from the config ticket, #3), which is the
reference if this page and it ever disagree. The server validates them at startup and exits with
a message naming each missing or invalid one.

Secret values must never be typed on a command line (they would land in the shell history) or
printed on screen. `fly secrets import` reads `NAME=VALUE` lines from stdin, so each value goes
from a hidden prompt or a generator straight to Fly:

```sh
# Neon direct connection string: paste it at the silent prompt.
read -rs DB_URL && printf 'DATABASE_URL=%s\n' "$DB_URL" | fly secrets import --stage; unset DB_URL

# Not a secret, so it can be typed as is.
fly secrets set --stage APP_BASE_URL='https://iron-oxide.fly.dev'
```

`fly secrets import` parses its input like a `.env` file: a value is cut at its first `#` and
surrounding `"` quotes are stripped. So a value must contain neither: in `DATABASE_URL`,
percent-encode any special character in the password (`#` is `%23`), as a URL requires anyway.
Base64 session keys and Google client secrets are unaffected.

`--stage` stores them without deploying (the app has no machine yet); the first deploy picks them
up. After the first deploy, drop `--stage`: Fly then restarts the machines with the new values.

For sign-in (#5), the same pattern applies (with `--stage` to set several before one
restart, then `fly secrets deploy`). Generate the session key and send it straight to Fly, without
ever seeing it:

```sh
printf 'SESSION_KEY=%s\n' "$(openssl rand 64 | openssl base64 -A)" | fly secrets import --stage
read -rs GOOGLE_SECRET && printf 'GOOGLE_CLIENT_SECRET=%s\n' "$GOOGLE_SECRET" \
  | fly secrets import --stage; unset GOOGLE_SECRET
# ... the non-secret sign-in values with `fly secrets set --stage NAME=value`, then:
fly secrets deploy
```

| Key | Required | What |
|---|---|---|
| `DATABASE_URL` | yes | Neon **direct** connection string (host without `-pooler`) with `?sslmode=require` (decision #39). The pooled endpoint breaks sqlx prepared statements and migrations. |
| `APP_BASE_URL` | yes | Public URL: `https://iron-oxide.fly.dev` until the custom domain is live, then `https://iron-oxyde.com`. |
| `WEBAUTHN_RP_ID` | yes | Passkey relying party id: the bare domain, `iron-oxyde.com` (`iron-oxide.fly.dev` while on the Fly hostname). Passkeys are bound to it: those made on the Fly hostname do not carry over. |
| `WEBAUTHN_ORIGIN` | yes | The origin of `APP_BASE_URL`, e.g. `https://iron-oxyde.com` (the server refuses anything else). |
| `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` | yes | OAuth "Web application" client from the Google Cloud console (see docs/auth.md). |
| `GOOGLE_REDIRECT_URL` | yes | `APP_BASE_URL`'s origin + `/auth/google/callback`, e.g. `https://iron-oxyde.com/auth/google/callback`, also registered in the Google client. |
| `SESSION_KEY` | yes | Session cookie key, at least 64 random bytes, base64. Generate it and pipe it to `fly secrets import` as shown above; never print it. Use a key that exists nowhere else. |
| `RUST_LOG` | no | Log filter, e.g. `info,sqlx=warn`. Not a secret: it can go in `[env]` in `fly.toml`. |

The six sign-in variables are required since sign-in (#5): **set them before deploying it**, or
the new machines refuse to start (the old ones keep serving). `WEBAUTHN_ORIGIN` and
`GOOGLE_REDIRECT_URL` must match `APP_BASE_URL`, so change all three together when moving to the
custom domain.

`IP` and `PORT` are not secrets: they are set in `fly.toml` (`0.0.0.0`, `8080`).

List what is set (values are never shown) with `fly secrets list`.

### 4. First deploy

From a checkout of `main`:

```sh
fly deploy
```

flyctl builds the image on Fly's remote builder (amd64), creates the machines in `fra` and waits
for the `/healthz` check to pass. The first deploy creates **two** machines for redundancy: with
`min_machines_running = 1` one always runs and the other is suspended while idle (billed for its
disk only). For a single machine, run `fly scale count 1` afterwards.

Check it:

```sh
fly status
fly logs
curl -sS https://iron-oxide.fly.dev/healthz   # ok (liveness)
curl -sS https://iron-oxide.fly.dev/readyz    # database reachable
```

`https://iron-oxide.fly.dev` is served over HTTPS with Fly's certificate, and `force_https`
redirects plain HTTP to it.

### 5. Deploy from GitHub Actions

The deploy job uses the `production` GitHub environment. Restrict that environment to `main`, so
that jobs on other branches cannot read the deploy token. (Any workflow job that runs on `main`
and declares `environment: production` can still read it, so review workflow changes on `main`
accordingly.)

```sh
gh api -X PUT repos/guizmaii-opensource/Iron-Oxide/environments/production \
  -F 'deployment_branch_policy[protected_branches]=false' \
  -F 'deployment_branch_policy[custom_branch_policies]=true'
gh api -X POST repos/guizmaii-opensource/Iron-Oxide/environments/production/deployment-branch-policies \
  -f name=main -f type=branch
```

(Or in the GitHub UI: Settings, Environments, `production`, Deployment branches: `main` only.)

Then create a deploy token scoped to this app only, and store it as a secret **of the
`production` environment**, not as a repository secret:

```sh
fly tokens create deploy --app iron-oxide --name github-actions --expiry 8760h \
  | gh secret set FLY_API_TOKEN --env production --repo guizmaii-opensource/Iron-Oxide
```

The token goes straight from flyctl to GitHub without being printed. It expires after a year
(`8760h`); create a new one and run the same command to rotate it. Until the secret exists, the
deploy job succeeds with a "Deploy skipped" notice instead of failing. The job also refuses to
deploy anything but `main`, including manual runs.

Then trigger a deploy: push to `main`, or run the workflow by hand:

```sh
gh workflow run deploy.yml --repo guizmaii-opensource/Iron-Oxide --ref main
gh run watch --repo guizmaii-opensource/Iron-Oxide
```

### 6. Custom domain `iron-oxyde.com`

Do this once the Route 53 registration of `iron-oxyde.com` is complete. Until then, use
`https://iron-oxide.fly.dev`.

1. Make sure the app has an IPv6 and a shared IPv4 address (the first deploy normally allocates
   both):

   ```sh
   fly ips list
   # if missing:
   fly ips allocate-v6
   fly ips allocate-v4 --shared
   ```

2. Ask Fly for certificates for the apex and `www`:

   ```sh
   fly certs add iron-oxyde.com
   fly certs add www.iron-oxyde.com
   ```

   Each command prints the DNS records to create. **Copy the records from that output**, not from
   this page: the addresses and the challenge target are specific to the app.

3. In Route 53, hosted zone `iron-oxyde.com`, create:

   | Name | Type | Value |
   |---|---|---|
   | `iron-oxyde.com` | `A` | the app's shared IPv4, from `fly ips list` |
   | `iron-oxyde.com` | `AAAA` | the app's IPv6, from `fly ips list` |
   | `www.iron-oxyde.com` | `CNAME` | `iron-oxide.fly.dev` |
   | `_acme-challenge.iron-oxyde.com` | `CNAME` | the target printed by `fly certs add` |
   | `_acme-challenge.www.iron-oxyde.com` | `CNAME` | the target printed by `fly certs add` |

   The apex uses plain A and AAAA records, as Fly recommends. A Route 53 `ALIAS` record can only
   point at AWS resources or records in the same hosted zone, so it cannot target `*.fly.dev`,
   and a `CNAME` is not allowed at the apex. The `_acme-challenge` records let Fly issue and
   renew the Let's Encrypt certificates even before traffic reaches the app.

4. Wait for the certificates:

   ```sh
   fly certs check iron-oxyde.com
   fly certs check www.iron-oxyde.com
   ```

5. Switch the app to the domain (restarts the app):

   ```sh
   fly secrets set APP_BASE_URL='https://iron-oxyde.com'
   ```

   Also set the WebAuthn and Google redirect values for the domain (see the table above) and add
   the redirect URL to the Google OAuth client.

### 7. Verify HTTPS

```sh
curl -sSI http://iron-oxyde.com/ | head -3        # 301 to https://
curl -sS https://iron-oxyde.com/healthz            # ok
curl -sS https://iron-oxyde.com/readyz             # database reachable
curl -sSv https://iron-oxyde.com/ -o /dev/null 2>&1 | grep -E 'subject:|issuer:|expire'
```

Then open `https://iron-oxyde.com` in a browser and sign in.

## Configuration choices

- **Always one machine running** (`min_machines_running = 1`, `auto_stop_machines = "suspend"`):
  no cold start for the first request, and a second machine resumes in well under a second when
  needed. Cheaper alternative: `min_machines_running = 0` lets the last machine suspend too, for
  near-zero cost when nobody uses the app, at the price of a short resume after a quiet period.
- **Health checks: `/healthz` for Fly, `/readyz` for humans and deploys.**
  - `/healthz` is liveness: it answers `ok` without touching the database. Fly checks it every
    30 s and uses it to route traffic and to gate rolling deploys.
  - `/readyz` also checks the database connection. The deploy workflow calls it once after each
    deploy, and you can call it by hand: `curl -sS https://iron-oxide.fly.dev/readyz`.
  - Never point a Fly check at `/readyz`: a database query every 30 s would keep the Neon compute
    awake around the clock (about 180 CU-hours a month, above the Free plan's quota).
- **VM**: `shared-cpu-1x` with 512 MB. Scale with `fly scale memory 1024` or
  `fly scale vm shared-cpu-2x` if needed.
- **No volume**: all state lives in Neon.
- **Signals**: `fly.toml` sets `kill_signal = "SIGTERM"` and `kill_timeout = "30s"`, so a machine
  being replaced or stopped gets SIGTERM and 30 s to drain in-flight requests (graceful shutdown,
  #3 / #4) before it is killed. Fly's defaults are SIGINT and 5 s. `--init` in the local
  `docker run` example forwards Ctrl-C promptly; it is harmless with graceful shutdown.

## Day-to-day

| Task | Command |
|---|---|
| Deploy the current checkout by hand | `fly deploy` |
| Logs | `fly logs` |
| Machines and health checks | `fly status`, `fly checks list` |
| Roll back to the previous image | `fly releases --image`, then `fly deploy --image <previous image>` |
| Shell on a machine | not available: the image is distroless (no shell) |
