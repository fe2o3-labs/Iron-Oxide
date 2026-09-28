# Rate limiting

The server limits how often one client can call the sign-in routes and the functions that write
(#23). The limits are counted in memory on each machine. A refused request gets `429 Too Many
Requests` with a `Retry-After` header.

Code: `crates/iron-oxide-app/src/server/rate_limit.rs` (groups, limits, middlewares),
`rate_limit/limiter.rs` (the bounded token bucket) and `rate_limit/client_ip.rs` (client IPs). The
client side of the contract is `crates/iron-oxide-app/src/rate_limit.rs`.

## What is limited

Each request belongs to at most one **group**. Each group has its own buckets, so using up one
group's limit does not affect another's.

| Group | Routes | Per IP | Per signed-in user |
|---|---|---|---|
| `auth_begin` | `passkey/sign-up/begin`, `passkey/sign-in/begin`, `passkey/add/begin`, `google/begin` | 30 at once, then 1 every 2 s | 10 at once, then 1 a minute |
| `auth_finish` | `passkey/sign-up/finish`, `passkey/sign-in/finish`, `passkey/add/finish` | 30 at once, then 1 every 2 s | 10 at once, then 1 a minute |
| `google_callback` | `GET`/`HEAD /auth/google/callback` | 30 at once, then 1 every 2 s | none |
| `session` | `auth/me`, `auth/sign-out` | 300 at once, then 5 a second | none |
| `account` | `passkey/remove`, `google/unlink` | 60 at once, then 1 a second | 10 at once, then 1 a minute |
| `write` | every other `POST`, `PUT`, `PATCH` or `DELETE`, on any path | 600 at once, then 10 a second | 120 at once, then 2 a second |

Route paths are under `/api/auth/` unless shown in full. Requests with a safe method (`GET`, `HEAD`,
`OPTIONS`, `TRACE`) are never limited, except the Google callback. That keeps page loads, assets,
`GET` server functions and the health checks (`/healthz`, `/readyz`) out of it.

The per-user limits only apply to requests whose session holds a user. A signed-out request only
counts against its IP.

### Where the checks run

The layers, from the outside in:

1. the CSRF check;
2. the per-IP limit;
3. the session layer;
4. the per-user limit;
5. the handler.

- **Before the session and the database.** The per-IP limit runs before any of them, so a refused
  request loads no session, writes no row and sets no cookie.
- **After the CSRF check.** Otherwise a cross-site page, opened by anyone behind a shared IP, could
  fire no-cors `POST`s at the begin functions. The CSRF check refuses them anyway, but they would
  use up the whole IP's sign-in limits and lock everyone behind it out.
- **The Google callback.** It is a cross-site `GET` by design, so such a page can still hit it
  (with an `<img>`, say). That is why the callback has its own bucket: the worst such a page can
  do is slow down Google callbacks for that IP, never passkey sign-ins. This is tested by
  `cross_site_requests_cannot_use_up_a_shared_ips_limits`.

### Why these numbers

- **Shared IPs.** A gym's Wi-Fi or a mobile carrier's NAT puts a whole room behind one IP address.
  The per-IP sign-in limits let a class of about 30 people sign in at once: one sign-in takes one or
  two begins and one finish. After that, each IP gets one begin every 2 s, which is plenty for
  people and slow for a script.
- **The known exposure (#59's reviews).** Each cookie-less begin creates one `sessions` row and one
  `auth_ceremonies` row, and the cleanup runs only every 6 hours. Without limits, 1,000 requests a
  second meant about 21.6 M rows of each between two cleanups. Now each IP can create at most 30
  rows at once, then 1 every 2 s: about 10,800 per IP between two cleanups. An IPv6 "IP" here is a
`/64` (see below); a client that holds a bigger block, such as a `/48`, has one set of limits per
`/64` in it. The per-IP check runs
  before the session is loaded, so a refused request writes nothing and sets no cookie (tested by
  `a_limited_begin_creates_no_session_and_no_ceremony`).
- **Google polling.** While a Google sign-in is open, the UI calls `me` every 2 s. That is 0.5 a
  second per person, so the `session` group allows 5 a second per IP for a room of people.
- **Offline sync.** The client retry queue (#30) sends a whole workout's logged sets at once when it
  comes back online. So `write` allows 120 at once per user, and 600 at once per IP for a room of
  people doing that together.
- **Account changes** (adding a passkey, linking or unlinking Google) are rare: 10 in a row per user
  is more than a real session needs.

### Per cookie?

There is no per-cookie limit. The cookie comes from the client: a script can drop it and get a new
session on each request, so a per-cookie limit is easy to get around. The per-IP limit covers both
cases: cookie-less begins, and concurrent begins on one cookie (which can each leave a ceremony
row).

## Client IP

`CLIENT_IP_SOURCE` says where the client's address comes from:

- **`peer`** (the default): the TCP peer address. It cannot be spoofed, and it is right when
  clients connect directly (local development, a plain VM). Headers such as `Fly-Client-IP` or
  `X-Forwarded-For` are ignored.
- **`fly`** (set in `fly.toml`): behind Fly.io, every connection comes from Fly's proxy, from a
  private `172.16.x.x` address. With `peer`, every user would share one set of limits. In this mode
  the server reads `Fly-Client-IP`, which [Fly's proxy always
  sets](https://fly.io/docs/networking/request-headers/) to the address it saw, overwriting what the
  client sent. It only trusts the header when all of these hold:
  - the TCP peer is a private address: loopback, `10/8`, `172.16/12`, `192.168/16`,
    `100.64/10`, link-local, or IPv6 unique-local (`fc00::/7`, Fly's private network) or link-local.
    A connection from the internet cannot set its own bucket;
  - the header appears exactly once and holds one valid IP address.

  Otherwise the peer address is used. `X-Forwarded-For` is never read.

**Residual risk in `fly` mode:** another machine on the organisation's private network (another
Fly app in the same org, or `fly proxy`) connects from a private address and could send any
`Fly-Client-IP`. Only the organisation's own apps and members can do that.

**If a CDN or another proxy is ever put in front of Fly,** `Fly-Client-IP` becomes that proxy's
address and all users behind it share one set of limits. The client IP would then have to come
from that proxy's own header, which needs a new mode.

**IPv6** clients are keyed by their `/64` prefix: one host usually holds a whole `/64` and could
otherwise rotate addresses to get fresh limits. IPv4-mapped IPv6 addresses are keyed as IPv4.

## Memory

Each limiter is a token bucket (GCRA) that keeps one timestamp per key (IP or user) in a hash map.
A key whose bucket has refilled carries no state, so dropping it changes nothing. The number of
keys is capped (50,000 per limiter, a few megabytes in total), by count and not by time:

- when a new key arrives and the table is full, the refilled keys are dropped first;
- if that frees too little, the keys closest to a full bucket are dropped. Keys that are being
  limited are kept.

Each such sweep frees at least an eighth of the table, so its cost is spread over many requests. A
flood of new addresses cannot grow the memory, and the most it can do is reset the limits of the
least-limited clients (tested by `memory_stays_bounded_under_many_client_ips`). A refused request
does not move its bucket further out, so hammering a limit does not lengthen it.

## Several machines

The counts live in each machine's memory. Fly can send one client's requests to different
machines, so with `N` machines running a client can get up to `N` times each limit. A restart or
deploy also resets the counts. This is accepted for now: the app runs one machine most of the time
(`min_machines_running = 1`, the others are suspended when idle). A shared store, such as a
Postgres table or Redis, would be needed for exact limits across machines.

## The 429 contract

Every refused request gets:

- status `429 Too Many Requests`;
- `Retry-After: N`: whole seconds, rounded up, at least 1. It is the time until the next request
  would be allowed;
- `Cache-Control: no-store`.

For server functions (paths under `/api/`) the body is the same JSON a server function's own error
has:

```json
{"message": "Too many requests. Please try again in 5 seconds.", "code": 429, "data": {"retry_after_secs": 5}}
```

The Dioxus client decodes it as `ServerFnError::ServerError { code: 429, message, details:
Some({"retry_after_secs": N}) }`, so the client can read the delay without the header. The
message can be shown as it is (the account page does). The Google callback page gets the same
message as plain text.

**For the client error classification (#68) and the retry queue (#30):**

- a 429 is **retryable, after the delay and never sooner**;
- read the delay with `crate::rate_limit::retry_after(&error)`. It returns `Some(delay)` for a 429:
  `retry_after_secs` from `details`, or 30 s if the 429 arrived without its body. It returns `None`
  for any other error;
- when you retry a batch, wait the delay once for the whole batch, not per request.

## Giving a new write endpoint stricter limits

Every new `POST` server function already gets the `write` limits, per IP and per user. For an
endpoint that needs stricter ones (an import, an upload, deleting the account), in
`server/rate_limit.rs`:

1. add a variant to `RouteGroup` (and to `RouteGroup::ALL`);
2. add its `GroupLimits` to `Limits` and set them in `Limits::default()`. Set `per_user` for a
   per-user limit, and keep a `per_ip` limit as well. Declare each quota as a `const` there, so an
   invalid one fails the build instead of panicking at startup;
3. map the endpoint's path to the group in `ROUTES`;
4. add a test that bursts past the new limit (see `rate_limit/integration_tests.rs`).

`every_route_in_the_table_exists` fails if a path in `ROUTES` is not a real route, so renaming an
endpoint cannot silently drop its limit.

## Tests

- `rate_limit::limiter::tests`: bursts, refill, `Retry-After` rounding, the key cap and which keys
  are dropped.
- `rate_limit::client_ip::tests`: the IP policy, private ranges, IPv6 `/64` keys.
- `rate_limit::integration_tests`:
  - a burst past the limit gets 429 with `Retry-After` until the bucket refills (paused clock);
  - limits are per IP and per group;
  - cross-site requests cannot use up a shared IP's limits;
  - a spoofed `Fly-Client-IP` is ignored in `peer` mode, and in `fly` mode from a public peer;
  - memory stays bounded under thousands of IPv4 and IPv6 clients;
  - the health checks are never limited;
  - every path in `ROUTES` exists;
  - with Postgres: a refused begin creates no rows, per-user limits are independent behind one IP,
    the 429 body matches the contract, and the default limits let 20 sign-ups from one IP through.
- `tests/startup.rs::the_sign_in_limit_is_per_client_behind_the_proxy`: the real binary in `fly`
  mode. It checks that the per-IP limit sees the real connection.
