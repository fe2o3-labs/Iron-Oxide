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
  and nothing is cached. The only exception is `server_time` (`GET /api/server-time`), which
  predates the conventions and reads nothing of the user's.
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
| `InvalidProgram(problems)` | 422 | `This program is not valid.` | An uploaded program document that does not parse or breaks a rule. `problems` (`ProgramProblems`) is sent as the error details, see [Programs](#programs-srcapiprogramsrs-19). |
| `TooLarge(msg)` | 413 | `msg` | A request body or document past its size limit |
| `Transient(detail)` | 503 | `The server is busy. Please try again.` | The same request can simply be retried: it may have been saved before a dropped connection, but every write is idempotent. Covers a concurrent write, a pool timeout, a dropped connection, a serialization failure or a deadlock. |
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

Every failed `/api/` call that answers JSON has the same body, whatever produced it:

```json
{ "message": "Not found.", "code": 404, "data": { "ServerError": { "message": "Not found.", "code": 404 } } }
```

`data.ServerError` may also have `details`, structured data for the UI (a list of problems, a
429's `retry_after_secs`). The Dioxus client decodes `data` into `ServerFnError::ServerError {
message, code, details }`: **our message and details are that variant's own `message` and
`details`**.

Dioxus produces other shapes on its own. An error returned by a server function has Dioxus's
`Display` text (`error running server function: …`) as the top `message`. An extractor rejection
(`AuthUser`'s 401, the CSRF 403) or arguments that do not decode give `{"error": text}`. A body
with a `data` that is not a `ServerError` (see the 429 below) would not decode on the client.
`server::api::errors_layer` rewrites all of them into the shape above. Bodies that are not JSON
(a panic, axum's own 405, 413 or 415) are left alone; the client classifies them by status.

**Arguments that do not decode** are `422 Invalid request.` They include a malformed id, a wrong
type or a missing field. Dioxus answers them with a `500` whose text is a serde error.

**Every 5xx except 503** gets the generic message and no details, whatever the function put in
it (`ServerFnError::new(detail)`, an `anyhow` error). The original text is logged.

**429 (rate limiting, #72).** The body is

```json
{ "message": "Too many requests. Please wait a moment.", "code": 429,
  "data": { "ServerError": { "message": "Too many requests. Please wait a moment.", "code": 429,
                             "details": { "retry_after_secs": 30 } } } }
```

plus the `Retry-After: 30` header. A limiter may also send `{"message", "code": 429, "data":
{"retry_after_secs": 30}}`: the layer moves that `data` into `details`. On the client,
`ApiFailure::retry_after_secs()` reads it.

### On the client

`crate::api::error::ApiFailure::classify(&ServerFnError)` gives a `FailureKind`, the message to
show and any structured `details`:

- It handles a decoded `ServerError { message, code, details }` and a bare
  `RequestError::Status(_, code)`.
- It shows **our** message, the `ServerError`'s own `message`, for every 4xx and for 503. For 500s
  and unknown statuses, and for answers that are not ours (`message` = `HTTP {code}: {text}`, the
  client's fallback for a body that is not our JSON), it shows a generic message for the kind and
  drops the details.
- 413 counts as `Invalid`.
- **Retryable:** `Transient` (503, 502, 504), `RateLimited` (429, honouring `Retry-After`), and
  `Network` (timeouts, connection failures, the request never answered).
- **Not retryable:** 400, 401, 403, 404, 409, 413, 422 and 500. Retrying the same request cannot fix
  them. A 401 means going back to sign-in.

## Idempotency

Anything the client creates gets its id on the client, a UUIDv7 from the domain's `new_v7()`, so a
retried request is recognised:

- same id, same content: success, and nothing changes (the repository reports `Change::Unchanged`);
- same id, different content: `409`;
- concurrent duplicates: one row, the others get the same success, or a `503` that a retry turns
  into it.

**Every write endpoint, updates included, must succeed unchanged when replayed.** A `503` can
follow a write that was committed (the connection dropped during `COMMIT`), and the retry queue
then sends it again.

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
  - `409` when another session is in progress: the user must finish or abandon it first. The
    database enforces it too, with a partial unique index (`workout_sessions_one_in_progress_idx`):
    of two devices starting different sessions at the same instant, exactly one succeeds and the
    other gets the same `409`.
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
  whose time is outside what can be stored (before 4714 BC, Postgres' earliest `timestamptz`, or
  after the year 9999) is `422`; another user's cursor just gives an empty page. At every page
  size, including the largest, `next` is set whenever another session follows.
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
| `/api/settings/update` | `settings: SettingsUpdate` | The saved `Settings` (plates sorted heaviest first). A full replace, so a retry is harmless. Concurrent updates (two devices) are last-writer-wins: the row always holds one whole update, never fields mixed from two. |
| `/api/settings/training-maxes` | none | The user's `TrainingMax`es, by exercise id. |
| `/api/settings/training-max/set` | `exercise_id`, `weight` (kg) | The saved `TrainingMax`. |
| `/api/settings/training-max/delete` | `exercise_id` | Nothing; `404` if the user has no training max for it. |

- **Validation (`422`, with the reason).** `SettingsUpdate` carries the values the user types
  unchecked: `bar_weight` and each plate as kg numbers (the same JSON as a `Weight`), the plate
  inventory as a plain list. The server validates them with the domain (`Weight::from_kg`,
  `PlateInventory::new`: no zero, duplicate or off-grid plate, at most 50 pairs and 16 sizes), and
  the default rest must be at most one hour. Weights out of range get a fixed message ("… must be between 0 and 2000 kg."), never the number echoed back. A typed `Weight` or `PlateInventory` argument would
  fail while the body is decoded, before the function runs, and only give the generic
  `422 Invalid request.` without saying which value is wrong.
- **Defaults only when nothing was saved.** A user who saves an empty plate inventory keeps an
  empty one; the defaults apply only while there is no `user_settings` row
  (`settings::find` returns `None`).
- **Training maxes and the progression anchor.** Setting a training max always moves its `set_at`
  to now, on the server's clock, even when the weight is unchanged: the progression (#57) restarts
  from this value and replays only the sets logged after it. This is the one write whose time
  comes from the server, since the point is "from now on". A retried request moves the anchor by a
  few seconds, which only matters if a set was logged in between. The weight must be more than
  zero.

## Programs (`src/api/programs.rs`, #19)

Every function is a `POST` that needs a signed-in user and only reads or changes that user's
programs. Built-in programs are read-only: a user trains with a copy, which is their own program.
An id of another user's program, or of a built-in's own row, gets the same `404` as an id that does
not exist.

| Function | Route | Arguments | Returns | Errors |
|---|---|---|---|---|
| `list_builtin_programs` | `/api/programs/builtins` | | `Vec<BuiltinProgramView>` (`builtin_id`, name, version, parsed document) | |
| `copy_builtin_program` | `/api/programs/copy-builtin` | `builtin_id`, `creation_id` | `ProgramDetail` of the copy | 404 unknown (or malformed) built-in id; 409 `creation_id` already used for another request |
| `list_programs` | `/api/programs/list` | `include_archived` | `Vec<ProgramView>`, oldest first | |
| `get_program` | `/api/programs/get` | `program_id` | `ProgramDetail` (latest version) | 404 |
| `get_active_program` | `/api/programs/active` | | `Option<ProgramDetail>` (latest version) | |
| `set_active_program` | `/api/programs/active/set` | `program_id` | `ProgramDetail` | 404; 409 archived |
| `upload_program` | `/api/programs/upload` | `target`, `document` | `UploadOutcome` (`program`, `version`, `saved`) | 404; 409 `creation_id` reused; 413; 422 `InvalidProgram` with `ProgramProblems` |
| `list_program_versions` | `/api/programs/versions` | `program_id` | `Vec<VersionView>`, oldest first, without documents | 404 |
| `set_program_archived` | `/api/programs/archive` | `program_id`, `archived` | `()` | 404; 409 archiving the active program |

- **Idempotency.** A copy and a new-program upload take a client `creation_id` (UUIDv7): a retry
  returns the program already created (`saved: false` for an upload); the same `creation_id` with a
  different built-in or document is `409`. A new version whose document equals the program's latest
  version adds nothing and returns that version with `saved: false`; documents are compared as
  jsonb, so formatting and key order do not matter.
- **Uploads are untrusted.** `target` is `{"kind": "new_program", "creation_id": …}` (named after
  the document's `name`) or `{"kind": "new_version", "program_id": …}` (the program keeps its own
  name). In order:
  1. A middleware on the route first checks the session (a signed-out client gets its `401`
     without the body being read), then reads the whole body before Dioxus does and refuses it
     with `413` past `UPLOAD_BODY_LIMIT` (2 × `MAX_DOCUMENT_BYTES` + 16 KiB: the document travels as a JSON
     string, where `"`, `\` and line breaks take two bytes). It checks `Content-Length` first and
     then counts the bytes actually read, so a missing or lying header does not get past it.
  2. A document over `MAX_DOCUMENT_BYTES` (256 KiB) is refused with `413`, before parsing.
  3. `Program::from_json` parses and validates it. A failure is `422` with `ProgramProblems`
     (`{errors: [{path, message, line?, column?}], omitted}`) as the error details: a parse error
     is one entry with its line and column; broken rules are at most `MAX_REPORTED_ERRORS` entries,
     the rest counted in `omitted`. Read them on the client with `ProgramProblems::from_error`.
     The rules include texts: names must not contain any C0 control character (U+0000 to
     U+001F), descriptions and notes none but tab, line feed and carriage return. Postgres cannot
     store U+0000, so this also keeps such a document from reaching the database.
  4. The document is stored as uploaded (like the built-ins), not re-serialized.
- **Archive, never delete.** Archiving hides a program from `list_programs` (unless
  `include_archived`) and keeps its versions and the sessions run from them; it can still be read
  and get new versions, and `archived: false` restores it. The active program cannot be archived and
  an archived program cannot be made active (`409`), so the active program is never hidden. Both
  calls check and write in one transaction that first locks the program's row, so concurrent calls
  on the same program run one after the other (one wins, the other gets `409`). Triggers enforce
  the rule in the database too (migration `20260928220000_active_program_never_archived`).
- **Timestamps.** `created_at` in `ProgramView` and `VersionView` is a `Timestamp` (milliseconds,
  see [Layout](#layout)).
