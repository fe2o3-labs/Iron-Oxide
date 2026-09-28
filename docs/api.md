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
| `InvalidProgram(problems)` | 422 | `This program is not valid.` | An uploaded program document that does not parse or breaks a rule. `problems` (`ProgramProblems`) is sent as the error details, see [Programs](#programs-srcapiprogramsrs-19). |
| `TooLarge(msg)` | 413 | `msg` | A request body or document past its size limit |
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
- **Not retryable:** 400, 401, 403, 404, 409, 413, 422 and 500. Retrying the same request cannot fix
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
