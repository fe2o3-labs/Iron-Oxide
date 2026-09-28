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

## Endpoints

### Sessions and sets (#18, `src/api/sessions.rs`)

| Function | Route | What it does |
|---|---|---|
| `start_session(session_id, started_at)` | `/api/sessions/start` | Starts a session of the active program's latest version, on the next day of its rotation. Returns a `SessionView`. |
| `get_session(session_id)` | `/api/sessions/get` | One session (`SessionView`) |
| `get_in_progress_session()` | `/api/sessions/in-progress` | The most recently started session in progress, with its sets in the order they were completed, or `null` |
| `get_next_session_plan()` | `/api/sessions/next-plan` | Today's plan before starting: the next day of the active program (latest version) and its targets from every completed session. `409` with no active program. |
| `get_session_plan(session_id)` | `/api/sessions/plan` | The session's day (name, exercises in program order). For each exercise: its definition in the session's version, and the progression engine's `NextTargets`, computed from the history before the session. |
| `save_set(session_id, set)` | `/api/sessions/save-set` | Logs a `LoggedSet<Timestamp>` |
| `finish_session(session_id, outcome, finished_at)` | `/api/sessions/finish` | Ends the session (`completed`, `skipped` or `abandoned`) and returns a `SessionSummary` |

Rules:

- **Starting.** The day is `next_day(rotation, history)`, where the history is every session of
  the active program, across all its versions.
  - A retry (same id, same `started_at`) returns the session it created, even after it ended. The
    day is never recomputed.
  - The same id with another `started_at` is `409`.
  - `409` when another session is in progress: the user must finish or abandon it first. Two
    devices starting at the same instant can both get through. The user then ends one of the two
    sessions.
  - `409` with no active program.
  - `409` for a rotation that repeats a day. Programs allow it, but `next_day` does not support it
    yet.
- **Sets.** The domain's `SessionLog::add_set` decides first; then the repository's upsert settles
  races.
  - The same set again is `200`, even after the session ended.
  - The same id with other values is `409`, also when that id was logged in another session.
  - A new set in an ended session is `409`.
  - A set completed before the session started is `422`.
  - Values the database refuses (a weight above the limit) are `422`.
  - `set_index` numbers the sets of one exercise and kind (warm-up or working) from 0. The
    prescribed working sets are `0..n`. Extras (a top single, back-off sets) are `n` and up. A
    skipped working set leaves a gap.
  - The exercise does not have to be on the session's day, so an added exercise is fine. Only the
    day's exercises get targets and progression.
- **Finishing.** The domain's `SessionLog::end` checks the time: not before the start or before a
  logged set (`422`). A retry with the same outcome and time returns the same summary. Another
  outcome or time is `409`. The summary is computed from stored data up to and including the
  session, so a retry made later gets the same one, unless the training max or the unit changed in
  between: `changes` and `needs_training_max` depend on them. It contains:
  - `volume`: the working sets, weighted and not timed, through `From<&LoggedSet> for
    Option<PerformedSet>`.
  - `prs`: completed sessions only. They are compared with every earlier completed session of
    **any** program.
  - `changes`: completed sessions only. The `ProgressionChange` for each exercise of the day that
    the session has working sets of.
  - `needs_training_max`: the day's exercises loaded as a percentage of a training max the user
    has not entered.
- **History given to the progression engine**, as agreed on #12 and #18:
  - completed sessions of the session's program, all versions, that started before the planned
    session (by start, then id);
  - each session judged against its own prescription, meaning the exercise on its day in the
    version it was run from;
  - for a training-max exercise, only the sets completed after the training max's `set_at`;
  - the training max the engine returns is only displayed. It is never stored.

  A stored version that no longer parses leaves its sessions unjudged (`Prescription` `None`).
  Stored sets that fall outside their session's time span, because of client clocks or a set saved
  while its session was finishing, have their time clamped into the span for the domain. The time
  plays no part in the rules applied to stored data, and refusing them would block the session for
  good.

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
enforces a floor, so raise the floor in `scripts/check-postgres-tests.sh` when you add some. For each of A's ids that an
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
