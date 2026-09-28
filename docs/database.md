# Database

Postgres 18 (Neon in production, Docker Compose locally). The migrations are in
`crates/iron-oxide-app/migrations/` and run at server startup; the repository layer that reads and
writes them is `crates/iron-oxide-app/src/server/db/`. See the README for running Postgres, adding
migrations and refreshing `.sqlx/`.

**Data isolation is the app's first security property: a user can never read or change another
user's data.** It is enforced twice: by the database schema (so a wrong query cannot break it) and
by a repository API that cannot express an unscoped query.

## Schema

```mermaid
erDiagram
    users ||--o| user_settings : "has"
    users ||--o{ training_maxes : "has"
    users ||--o{ programs : "owns (NULL owner = built-in)"
    programs ||--|{ program_versions : "versions"
    users ||--o| active_program : "trains with"
    programs ||--o| active_program : "(program_id, user_id)"
    users ||--o{ workout_sessions : "logs"
    program_versions ||--o{ workout_sessions : "(program_version_id, user_id)"
    users ||--o{ workout_sets : "logs"
    workout_sessions ||--o{ workout_sets : "(user_id, session_id)"

    users {
        uuid id PK
        user_plan plan "free | pro"
        text display_name
        timestamptz created_at
    }
    user_settings {
        uuid user_id PK "FK users"
        text unit "kg | lb"
        bigint bar_weight_ng
        jsonb plate_inventory "array, at most 16 sizes"
        bigint default_rest_s
        boolean sound_enabled
        timestamptz updated_at
    }
    training_maxes {
        uuid user_id PK "FK users"
        text exercise_id PK "slug"
        bigint weight_ng
        timestamptz set_at "progression anchor"
    }
    programs {
        uuid id PK
        uuid user_id "NULL only for built-ins"
        text source_builtin_id "slug"
        text name "1-100 chars"
        boolean archived
        timestamptz created_at
    }
    program_versions {
        uuid id PK
        uuid program_id FK
        uuid user_id "copied from the program"
        integer version "1, 2, ... unique per program"
        jsonb document "program JSON, schema_version"
        timestamptz created_at
    }
    active_program {
        uuid user_id PK "FK users"
        uuid program_id "FK (program_id, user_id)"
        timestamptz updated_at
    }
    workout_sessions {
        uuid user_id PK "FK users"
        uuid id PK "client-generated, unique per user"
        uuid program_version_id "FK (program_version_id, user_id)"
        text day_id "slug"
        text status "in_progress | completed | skipped | abandoned"
        timestamptz started_at
        timestamptz finished_at "set iff ended"
    }
    workout_sets {
        uuid user_id PK "FK users"
        uuid id PK "client-generated, unique per user"
        uuid session_id "FK (user_id, session_id)"
        text exercise_id "slug"
        integer set_index "0-65535"
        integer reps "0-65535"
        bigint weight_ng "NULL = body weight"
        bigint duration_s "NULL = not timed"
        boolean warmup
        timestamptz completed_at
    }
```

Sign-in tables (`passkeys`, `oauth_identities`, sign-in `sessions`) come with #5 and follow the same
rules. The workout tables are called `workout_sessions` and `workout_sets` so they cannot clash
with the sign-in `sessions` table.

### Tables

| Table | Holds | Notes |
|---|---|---|
| `user_settings` | Unit, bar weight, plate inventory, default rest, sound | One row per user, created by the first save. Until then the repository returns `UserSettings::defaults()`, which a test keeps equal to the column defaults. |
| `training_maxes` | One training max per user and exercise, with `set_at` | Per user, not per program (#56). A table rather than a jsonb map, so the weight range and the slug are checked. `set_at` is when the lifter entered it: the progression engine (#57) replays the completed sets after it, so the engine's result is never stored back without moving `set_at` (the repository writes both together). |
| `programs` | A program's header | `user_id` is NULL only for built-ins (`CHECK (user_id IS NOT NULL OR source_builtin_id IS NOT NULL)`); one row per built-in id (partial unique index). A copy of a built-in keeps its `source_builtin_id`. Programs are archived, not deleted, so old sessions stay linked. |
| `program_versions` | Immutable program documents | `version` is 1, 2, ... per program. A trigger rejects every `UPDATE`; there is no update path in the repository. Rows are only deleted by cascade (program or user deleted). |
| `active_program` | The program a user trains with | Composite FK to `programs (id, user_id)`: only one of the user's own programs (never a built-in; copy it first). |
| `workout_sessions` | A training session | Client-generated id, primary key `(user_id, id)`. `finished_at` is set exactly when the status is not `in_progress`, and not before `started_at`. |
| `workout_sets` | A logged set | Client-generated id, the idempotency key; primary key `(user_id, id)`. |

### Mapping to the domain types

The repository does not depend on `iron-oxide-domain` yet: the domain types are in open PRs. Its
types mirror them field for field, and switch to them once they are merged.

| Column | Domain type | Storage |
|---|---|---|
| `*_ng` (`bar_weight_ng`, `weight_ng`) | `Weight` (#48) | Exact nanograms, `bigint`, `CHECK` 0 to 2 × 10¹⁵ (2000 kg, `Weight::MAX`). Never floats. |
| `reps`, `set_index` | `Reps` / `u16` (#48, #54) | `integer` with `CHECK` 0 to 65535 (`smallint` is too small for `u16`). |
| `duration_s`, `default_rest_s` | `Seconds` / `u32` (#48) | `bigint` with `CHECK` 0 to 4294967295 (`integer` is too small for `u32`). |
| `exercise_id`, `day_id`, `source_builtin_id` | `ExerciseId`, `DayId`, `BuiltinProgramId` (#48, #56) | `text`, `CHECK (is_slug(...))`: 1 to 64 of `[a-z0-9]` in words split by single hyphens. |
| `workout_sessions.id`, `workout_sets.id` | `SessionId`, `SetId` (#48) | `uuid`, generated on the client. |
| `programs.id`, `program_versions.id`, `users.id` | `ProgramId`, `ProgramVersionId`, `UserId` (#48) | `uuid`, generated by Postgres. |
| `status` | `SessionStatus` (#54) | `text`, `CHECK` in `in_progress`, `completed`, `skipped`, `abandoned` (the domain's serde names). |
| `started_at`, `finished_at`, `completed_at` | The session timestamp `T` (#54) | `timestamptz` (microseconds; the domain uses milliseconds, which fit exactly). |
| `document` | `Program` JSON (#56) | `jsonb`: an object with a numeric `schema_version`, at most 1 MiB. Validated by the domain before it is written. |
| `plate_inventory` | `PlateInventory` JSON (#53) | `jsonb` array of at most 16 entries. Validated by the domain before it is written. |

## Isolation strategy

### In the database

1. **Every user-owned table has `user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE`** and an
   index that starts with `user_id`. (`programs` and `program_versions` allow NULL, for built-ins
   only.) Deleting a user deletes all their rows in one statement.
2. **Client-generated ids are unique per user**: `workout_sessions` and `workout_sets` have the
   primary key `(user_id, id)`. Another user's id is exactly like a free one: no error, no
   "taken" signal, and two users may use the same UUID independently. Every unique key of a
   user-owned table includes `user_id`, except a short allowlist of keys over server-generated
   values (`programs`/`program_versions` ids from `gen_random_uuid()`, built-in ids, version
   numbers).
3. **Composite foreign keys that include `user_id`**: a row that points at another user-owned row
   must have the same owner.
   - `workout_sets (user_id, session_id)` → `workout_sessions (user_id, id)`
   - `workout_sessions (program_version_id, user_id)` → `program_versions (id, user_id)`
   - `active_program (program_id, user_id)` → `programs (id, user_id)`
   - `program_versions (program_id, user_id)` → `programs (id, user_id)`

   So a set cannot be attached to another user's session, a session cannot be run from another
   user's (or a built-in's) program version, and another user's program cannot be made active,
   whatever the application sends.
4. **Owners never change**: a trigger rejects any update of `user_id` in every user-owned table.
5. **Program versions take their owner from their program**: a trigger sets
   `program_versions.user_id` from `programs.user_id`, ignoring what the caller wrote. This also
   covers built-ins, where the composite key alone would not be checked (a NULL column skips a
   `MATCH SIMPLE` foreign key).
6. **Domain invariants as `CHECK`s**: weight, reps, duration and index ranges, slugs, session
   status, `finished_at` iff ended and not before the start, JSON shapes.

`sessions → program_versions` is `NO ACTION` (checked at the end of the statement), not
`RESTRICT`, so deleting a user can cascade to both tables in one statement.

### In the repository (`server/db/`)

- **Every function that touches a user's data takes the caller's `UserId`** and filters every
  statement by it (`WHERE ... AND user_id = $user`), including the follow-up reads of idempotent
  writes. Server functions pass the `UserId` from `AuthUser` (#5), never one sent by the client.
  The only unscoped functions are the built-in ones (`programs::seed_builtins`,
  `programs::list_builtins`), which only touch rows without an owner.
- **No existence leak**: a row that does not exist and a row that belongs to someone else give
  the same `RepoError::NotFound`. Foreign key violations (which, with the composite keys, mean
  "not one of yours") also map to `NotFound`. Error messages never include ids, values or
  database error text.
- **Idempotent writes on client ids** (`sessions::start`, `sets::upsert_idempotent`):
  `INSERT ... ON CONFLICT (user_id, id) DO NOTHING`, then, if nothing was inserted, compare with
  the caller's own row (`WHERE id = $id AND user_id = $user`):
  - same id and same content: `Change::Unchanged` (one row, safe under concurrent retries);
  - same id and different content: `RepoError::Conflict`;
  - an id another user also uses: irrelevant, the write is the caller's own and succeeds. The
    other row is never read, compared or modified.
- **Typed ids** (`UserId`, `ProgramId`, `ProgramVersionId`, `SessionId`, `SetId`) so ids of
  different kinds cannot be swapped. They will be replaced by the domain ids of #48.
- Queries use the compile-time checked `sqlx::query!` / `query_as!` macros; the metadata is in
  `.sqlx/`.

| Module | Functions |
|---|---|
| `settings` | `get` (defaults when never saved), `save` |
| `training_maxes` | `list`, `set`, `delete` |
| `programs` | `seed_builtins`, `list_builtins`, `copy_builtin`, `create`, `get`, `list`, `rename`, `set_archived`, `add_version` (a retried identical upload is a no-op), `list_versions`, `get_version`, `latest_version` |
| `active_program` | `get`, `set`, `clear` |
| `sessions` | `start` (idempotent), `finish` (idempotent), `get`, `get_in_progress`, `list` (history pages by `(started_at, id)`, optionally for one program) |
| `sets` | `upsert_idempotent`, `list_for_session`, `completed_for_exercise` (sets of one exercise after a time, in completed sessions of any version of a program: the progression input of #57, served by the `(user_id, exercise_id, completed_at)` index) |

Rules that span rows and are checked by the domain (`SessionLog`, #54) before a write, not by the
database: a set completed before its session started or after it ended.

## Built-in programs

`programs::BUILTIN_PROGRAMS` is the list seeded at every startup (`AppState::init`), after the
migrations. `seed_builtins` runs in one transaction under an advisory lock (two instances starting
together are fine). It creates missing built-ins, adds a version when a document changed,
un-archives the listed ones and archives built-ins that are no longer listed. The list is empty
until the program documents land (#56); it will then map the domain's `builtin_programs()` to
`BuiltinSeed { builtin_id, name, json }`.

## Tests

Tests that need Postgres are `#[sqlx::test(migrator = "MIGRATOR")]` + `#[ignore = "needs
Postgres"]`, inside the `server/db` modules (the app is a binary crate, so integration tests in
`tests/` cannot reach it). `sqlx::test` creates a fresh database per test from `DATABASE_URL`,
applies every migration and drops it afterwards, so tests are isolated and run in parallel.

```sh
docker compose up -d --wait
DATABASE_URL=postgres://iron_oxide:iron_oxide@localhost:5433/iron_oxide_test \
  cargo test -p iron-oxide-app --features server -- --ignored
```

`server/db/testing.rs` has the helpers: `users_a_and_b` (A owns the data, B tries to reach it),
`program`, `session`, `set` and `populate` (one row in every user-owned table).

- **Isolation, per repository function**: B cannot read, update or delete A's rows, and using A's
  real id gives the same error as a random id (`another_users_*_are_invisible_and_untouchable`,
  `users_only_see_and_change_their_own_*`, `nobody_can_change_a_builtin_through_the_repository`).
- **Idempotency**: same id and content is one row, different content is a conflict, two users
  using the same session and set ids both succeed independently, errors never contain ids, and
  concurrent duplicates (sets, sessions, version uploads) produce one row or consecutive version
  numbers.
- **Schema, with raw SQL that bypasses the repository** (`schema_tests.rs`): the composite keys,
  the owner and immutability triggers, every `CHECK`, and:
  - every `user_id` column in `public` has a cascading foreign key to `users` and an index that
    starts with it (read from `pg_constraint`/`pg_index`);
  - every table with a `user_id` column has a `BEFORE UPDATE` row trigger that forbids changing
    the owner (read from `pg_trigger`), and moving a user's rows to a user who has none fails on
    that trigger;
  - every table in `public` has a `user_id` column or is on a short allowlist (`users`,
    `_sqlx_migrations`);
  - deleting a user empties every table listed by `information_schema` that has a `user_id`
    column, and keeps the other user's rows. `populate` must write to each such table first, so a
    new table fails this test until it is covered;
  - every unique key (primary keys included) of a table with a `user_id` column includes
    `user_id`, or is on the `UNIQUE_KEYS_WITHOUT_OWNER` allowlist with a reason.

Not here yet: a test-only way to sign in as a user, and isolation tests at the server-function
level (including the GDPR export). They come with `AuthUser` (#5) and the server functions
(#18–#22), which wrap this repository.

CI runs them in the "Integration tests (Postgres)" job, and fails if the key isolation tests do not
appear as passed in its log.

### Adding a user-owned table

1. `user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE`, an index starting with `user_id`, and
   the `forbid_owner_change` trigger.
2. Client-generated ids in a `(user_id, id)` primary key; every other unique key including
   `user_id` too (or allowlisted in `schema_tests.rs` with a reason).
3. References to other user-owned rows as composite foreign keys including `user_id` (add a
   `UNIQUE (id, user_id)` on the target if needed).
4. Repository functions that take a `UserId` and filter every statement by it, returning
   `NotFound` for rows that are not the caller's.
5. Add a row to it in `testing::populate`, and isolation tests for each new function.
