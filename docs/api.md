# Server functions (API conventions)

Every server function follows the same rules (#68), so the client, the retry queue (#30) and the
tests can treat them all alike.

## Layout

| Where | What |
|---|---|
| `crates/iron-oxide-app/src/api.rs` | One `pub mod <area>;` line per area |
| `crates/iron-oxide-app/src/api/<area>.rs` | The area's `#[post]` server functions and the types they exchange (DTOs) |
| `crates/iron-oxide-app/src/api/error.rs` | Client side: `ApiFailure::classify` for the UI and the retry queue |
| `crates/iron-oxide-app/src/server/api/<area>.rs` | Server-only logic behind the area's functions, and its tests |
| `crates/iron-oxide-app/src/server/api/error.rs` | `ApiError` and its conversions |
| `crates/iron-oxide-app/src/server/api/errors_layer.rs` | The layer that gives every `/api/` error the same body |
| `crates/iron-oxide-app/src/server/api/testing.rs` | The endpoint test harness |

Sign-in (`src/auth/api.rs`, #5) predates this layout and keeps its own `AuthError`, with the same
mapping style.

A server function only extracts what it needs and calls the area's logic:

```rust
// src/api/sessions.rs
#[post("/api/sessions/get", state: Extension<AppState>, user: AuthUser)]
pub async fn get_session(session_id: SessionId) -> Result<SessionView, ServerFnError> {
    Ok(sessions::get(&state.db, user.owner(), session_id).await?)
}
```

- Every function is a `POST` under `/api/<area>/<name>`. The CSRF layer checks every `POST`,
  and nothing is cached.
- The server-only arguments go after the route: `state: Extension<AppState>` for the pool and
  `user: AuthUser` for the signed-in user. Without a valid session, `AuthUser` rejects the call with
  `401` before the body runs.
- **The user always comes from the session.** `user.owner()` is the repository's owner key. Never
  accept a user id from the client. Every repository call takes it and scopes every query by it
  (see `docs/database.md`).
- Arguments and results use the domain types: the typed ids (`SessionId`, …), `DayId`, `Weight`,
  and so on. Times are `iron_oxide_domain::time::Timestamp`, milliseconds since the Unix epoch in
  UTC, serialized as a JSON integer. `server::api::timestamp` and `server::api::offset_date_time`
  convert to and from the database's `timestamptz`.
- The logic returns `Result<_, ApiError>`, and `?` turns it into a `ServerFnError`.

## Errors

`ApiError` (server-only) becomes a `ServerFnError::ServerError { code, message }`. The message is
short, generic and safe to show. Details such as database errors, constraint names and ids are
logged, never returned.

| Variant | Status | Message | When |
|---|---|---|---|
| `NotFound` | 404 | `Not found.` | No such row **among the caller's own**. Another user's id gives exactly the same answer as an id that does not exist. |
| `Conflict(msg)` | 409 | `msg` | An id reused with different content, a session that has already ended |
| `Invalid(msg)` | 422 | `msg` | Invalid input: a domain value, a program document, a database `CHECK` (`Invalid value.`) |
| `Transient(detail)` | 503 | `The server is busy. Please try again.` | Nothing was saved and the same request can simply be retried. Covers a concurrent write, a pool timeout, a dropped connection, a serialization failure or a deadlock. |
| `Unauthorized` | 401 | `Please sign in.` | Not signed in (normally rejected earlier by `AuthUser`) |
| `Forbidden(msg)` | 403 | `msg` | Plan gating (#21) |
| `Internal(detail)` | 500 | `Something went wrong. Please try again.` | Bugs, corrupt stored data, any other database failure |

The conversions:

- `From<RepoError>`:
  - `NotFound` → 404
  - `Conflict` and `SessionEnded` → 409
  - `Transient` → 503
  - `Invalid` → 422
  - `Corrupt` → 500
  - `Database` → 503 if transient, 500 otherwise
- `From<AuthError>` keeps sign-in's status.
- `From<ValueError>` and `From<ProgramError>` → 422 with the domain's message, which only repeats what
  the user sent.
- `From<SessionError>`: 409 or 422 with a fixed message. The domain's own text names ids, so it is
  never used.

Messages written for users go in the variant. Anything else goes in the log.

`ApiError` is meant to grow: an area that needs another status adds a variant with its `public()`
status and message (and, for structured data such as a list of problems, `details` on the
`ServerFnError`). The client maps statuses, not variants.

### The error body

Every failed `/api/` call answers with the same JSON, whatever produced it:

```json
{ "message": "Not found.", "code": 404, "data": { "ServerError": { "message": "Not found.", "code": 404 } } }
```

Dioxus produces two shapes on its own. An error returned by a server function has Dioxus's
`Display` text (`error running server function: Not found. (details: None)`) as `message`. An
extractor rejection (`AuthUser`'s 401, the CSRF 403) or arguments that do not decode give
`{"error": text}`. `server::api::errors_layer` rewrites both into the shape above, with our message
on top and in `data.ServerError`, keeping any `details`.

**Arguments that do not decode** are `422 Invalid request.` They include a malformed id, a wrong
type or a missing field. Dioxus answers them with a `500` whose text is a serde error. Any other
raw `500` gets the generic message. The original text is logged in both cases.

### On the client

`crate::api::error::ApiFailure::classify(&ServerFnError)` gives a `FailureKind`, the message to
show and any structured `details`:

- It handles a decoded `ServerError { code, details, .. }` and a bare
  `RequestError::Status(_, code)`.
- The message shown is only ever **ours**, from `details.ServerError.message`, for every 4xx and
  for 503. It never shows the `ServerFnError`'s own `message` or `Display`, which can be Dioxus's
  text or a proxy's page. Without our message, and for 500s and unknown statuses, it shows a
  generic message for the kind.
- 413 counts as `Invalid`.
- **Retryable:** `Transient` (503, 502, 504), `RateLimited` (429, honouring `Retry-After`), and
  `Network` (timeouts, connection failures, the request never answered).
- **Not retryable:** 400, 401, 403, 404, 409, 422 and 500. Retrying the same request cannot fix
  them. A 401 means going back to sign-in.

## Idempotency

Anything the client creates gets its id on the client, a UUIDv7 from the domain's `new_v7()`, so a
retried request is recognised:

- same id, same content: success, and nothing changes (the repository reports `Change::Unchanged`);
- same id, different content: `409`;
- concurrent duplicates: one row, the others get the same success, or a `503` that a retry turns
  into it.

Timestamps that are part of what is saved (`started_at`, `completed_at`, `finished_at`) come from
the client, in the request. If the server stamped `now()`, a retry would carry a different time
and look like a conflict. The retry queue (#30) re-sends exactly the same body.

## Tests

Server functions are tested through the real router, as signed-in users, with
`server::api::testing`:

- `TestApi::new(db)` builds the full app over the fresh database that `#[sqlx::test]` provides.
- `api.user(name)` and `api.users_a_and_b()` sign users up with a software passkey (#5's test
  support). Each user has their own browser and cookie.
- `user.id` is the repository owner key, used to seed data with `server::db::testing`.
- `user.call::<T>(path, json!({ ... }))` sends a `POST` with the arguments as JSON and decodes the
  result, or returns a `CallError { status, message }`. `user.call_err(...)` expects a failure.
- `assert_not_found_for_other_user(&mut b, path, a_id, |id| json!({ ... }))` checks that B gets
  exactly the `404 Not found.` of an id that exists for nobody, both for A's id and for a random
  one.
- `assert_unauthorized_when_signed_out(&api, path, body)` checks for a `401`.

Tests that need Postgres go under `server::api::<area>::tests`, with
`#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]` and `#[ignore = "needs Postgres"]`. CI
checks that every one of them passed.

**Isolation tests are required for every endpoint.** Name them `another_users_*`. CI counts them and
enforces a floor, so raise the floor in `ci.yml` when you add some. For each of A's ids that an
endpoint accepts, B gets 404 from reads, updates and deletes, and nothing of A's appears in B's
lists. After a refused write, A's data is unchanged.

```rust
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_session_is_not_found(db: PgPool) {
    let api = TestApi::new(db).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    let session = db_testing::session(&api.db, a.id).await;
    let body = |id: Uuid| json!({ "session_id": id });

    testing::assert_not_found_for_other_user(&mut b, GET, session.as_uuid(), body).await;
    // A still sees it: the 404 was about B, not about the id.
    let view: Result<SessionView, _> = a.call(GET, body(session.as_uuid())).await;
    assert!(view.is_ok(), "{view:?}");
}
```

Unit tests without a database stay next to the code as usual.

## Endpoints

### History (`src/api/history.rs`, #20)

The history is the user's **ended** sessions (completed, skipped or abandoned). Weights are the
domain `Weight` (kg numbers on the wire); the UI converts them to the user's unit. e1RM uses the
Epley formula.

| Path | Arguments | Result |
|---|---|---|
| `/api/history/page` | `cursor: Option<HistoryCursor>`, `limit: Option<u32>` (default 20, 1 to 100) | `HistoryPage { sessions, next }`: ended sessions, most recently finished first (ties by id, descending). `next` is `None` on the last page. |
| `/api/history/session` | `session_id` | `SessionDetails`: the session (ended or still in progress) and its sets grouped by exercise (in the order each was first logged), with each exercise's top set, best e1RM and volume. |
| `/api/history/exercise-series` | `exercise_id` (a slug) | `ExerciseSeries`: one point per ended session with a weighted working set, oldest first: the top set and the best e1RM. |
| `/api/history/exercises` | none | The exercises logged in ended sessions, most recently trained first, with the number of sessions. |

- **Cursor.** `HistoryCursor` is opaque to the client: pass back the `next` of the previous page.
  It holds the last session's `finished_at` in **microseconds** (the database's precision) and its
  id. A millisecond cursor would skip sessions finished within the same millisecond. A cursor
  whose time is out of range is `422`; another user's cursor just gives an empty page.
- **Charts.** A point's key is the session's start time and id (`SeriesKey`), so two sessions
  started in the same millisecond stay apart. Sets without a weight (body-weight work) have no
  point; sets of abandoned sessions count (they were lifted). A session still in progress is left
  out until it ends.
- Errors: `422` for a page size out of range, a bad cursor or an exercise id that is not a slug;
  `404` for a session that is not the user's.

### Settings (`src/api/settings.rs`, #20)

| Path | Arguments | Result |
|---|---|---|
| `/api/settings/get` | none | `Settings`. A user who never saved any gets `Settings::defaults()`: kg, a 20 kg bar, the domain's default kg plate inventory (`PlateInventory::default_for(Kg)`), 120 s of rest, sound on. |
| `/api/settings/update` | `settings: SettingsUpdate` | The saved `Settings` (plates sorted heaviest first). A full replace, so a retry is harmless. |
| `/api/settings/training-maxes` | none | The user's `TrainingMax`es, by exercise id. |
| `/api/settings/training-max/set` | `exercise_id`, `weight` (kg) | The saved `TrainingMax`. |
| `/api/settings/training-max/delete` | `exercise_id` | Nothing; `404` if the user has no training max for it. |

- **Validation (`422`, with the reason).** `SettingsUpdate` carries the values the user types
  unchecked: `bar_weight` and each plate as kg numbers (the same JSON as a `Weight`), the plate
  inventory as a plain list. The server validates them with the domain (`Weight::from_kg`,
  `PlateInventory::new`: no zero, duplicate or off-grid plate, at most 50 pairs and 16 sizes), and
  the default rest must be at most one hour. A typed `Weight` or `PlateInventory` argument would
  fail while the body is decoded, before the function runs, which Dioxus reports as a `500` with
  the decoding error.
- **Defaults only when nothing was saved.** A user who saves an empty plate inventory keeps an
  empty one; the defaults apply only while there is no `user_settings` row
  (`settings::find` returns `None`).
- **Training maxes and the progression anchor.** Setting a training max always moves its `set_at`
  to now, on the server's clock, even when the weight is unchanged: the progression (#57) restarts
  from this value and replays only the sets logged after it. This is the one write whose time
  comes from the server, since the point is "from now on". A retried request moves the anchor by a
  few seconds, which only matters if a set was logged in between. The weight must be more than
  zero.
