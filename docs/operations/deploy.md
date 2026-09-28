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

```sh
docker build -t iron-oxide .
docker run --rm --init -p 8080:8080 iron-oxide
curl http://127.0.0.1:8080/healthz   # ok
open http://127.0.0.1:8080/
```

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

Set them in one command, so the app restarts only once. Values come from Neon, Google Cloud and a
generator, never from a file in the repo.

```sh
fly secrets set --stage \
  DATABASE_URL='postgres://USER:PASSWORD@ep-XXXX.eu-central-1.aws.neon.tech/DBNAME?sslmode=require' \
  APP_BASE_URL='https://iron-oxide.fly.dev'
```

`--stage` stores them without deploying (the app has no machine yet); the first deploy picks them
up. After the first deploy, drop `--stage`: `fly secrets set` then restarts the machines.

| Key | Required | What |
|---|---|---|
| `DATABASE_URL` | yes | Neon **direct** connection string (host without `-pooler`) with `?sslmode=require` (decision #39). The pooled endpoint breaks sqlx prepared statements and migrations. |
| `APP_BASE_URL` | yes | Public URL: `https://iron-oxide.fly.dev` until the custom domain is live, then `https://iron-oxyde.com`. |
| `WEBAUTHN_RP_ID` | sign-in | Passkey relying party id: the bare domain, `iron-oxyde.com`. Passkeys are bound to it, so set it once the custom domain is live. |
| `WEBAUTHN_ORIGIN` | sign-in | `https://iron-oxyde.com` |
| `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` | sign-in | OAuth "Web application" client from the Google Cloud console. |
| `GOOGLE_REDIRECT_URL` | sign-in | `https://iron-oxyde.com/auth/google/callback`, also registered in the Google client. |
| `SESSION_KEY` | sign-in | Session cookie key, at least 64 random bytes, base64: `openssl rand 64 \| openssl base64 -A`. Use a key that exists nowhere else. |
| `RUST_LOG` | no | Log filter, e.g. `info,sqlx=warn`. Not a secret: it can go in `[env]` in `fly.toml`. |

The six sign-in variables are all-or-nothing: set all of them or none (they are needed once
sign-in, #5, lands). The hello-world scaffold needs none of these; `DATABASE_URL` and
`APP_BASE_URL` become required when the config and database work (#3 / #4) is merged.

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
curl -sS https://iron-oxide.fly.dev/healthz   # ok
```

`https://iron-oxide.fly.dev` is served over HTTPS with Fly's certificate, and `force_https`
redirects plain HTTP to it.

### 5. Deploy from GitHub Actions

Create a deploy token scoped to this app only, and store it as a repository secret:

```sh
fly tokens create deploy --app iron-oxide --name github-actions --expiry 8760h \
  | gh secret set FLY_API_TOKEN --repo guizmaii-opensource/Iron-Oxide
```

The token goes straight from flyctl to GitHub without being printed. It expires after a year
(`8760h`); create a new one and run the same command to rotate it. Until the secret exists, the
deploy job succeeds with a "Deploy skipped" notice instead of failing.

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
curl -sSv https://iron-oxyde.com/ -o /dev/null 2>&1 | grep -E 'subject:|issuer:|expire'
```

Then open `https://iron-oxyde.com` in a browser, sign in, and check `/healthz`.

## Configuration choices

- **Always one machine running** (`min_machines_running = 1`, `auto_stop_machines = "suspend"`):
  no cold start for the first request, and a second machine resumes in well under a second when
  needed. Cheaper alternative: `min_machines_running = 0` lets the last machine suspend too, for
  near-zero cost when nobody uses the app, at the price of a short resume after a quiet period.
- **Health check on `/healthz` every 30 s.** If `/healthz` starts querying the database (#4),
  every check wakes the Neon compute, which then never scales to zero and uses the Free plan's
  compute hours around the clock. In that case, either point the Fly check at a liveness path
  that does not touch the database, or accept an always-on Neon compute.
- **VM**: `shared-cpu-1x` with 512 MB. Scale with `fly scale memory 1024` or
  `fly scale vm shared-cpu-2x` if needed.
- **No volume**: all state lives in Neon.
- **Signals**: the server has no signal handler yet, so as PID 1 in a plain `docker run` it
  ignores `SIGTERM`/`SIGINT` (hence `--init` above). On Fly the app runs under Fly's own init and
  is stopped normally, but in-flight requests are not drained; graceful shutdown belongs in
  `main.rs`.

## Day-to-day

| Task | Command |
|---|---|
| Deploy the current checkout by hand | `fly deploy` |
| Logs | `fly logs` |
| Machines and health checks | `fly status`, `fly checks list` |
| Roll back to the previous image | `fly releases --image`, then `fly deploy --image <previous image>` |
| Shell on a machine | not available: the image is distroless (no shell) |
