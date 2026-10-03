# Account export format (#22)

`export_account_data()` (`POST /api/account/export`) returns everything the signed-in user owns as
one JSON document. `import_account_data(document)` (`POST /api/account/import`) takes the same
document back, as the JSON text of the file. The types are `ExportDocument` and its parts in
`crates/iron-oxide-app/src/api/account.rs`. This page is their reference.

The export holds the user's own data only: never another user's, never a sign-in session, never
passkey key material.

## Example

```json
{
  "format": "iron-oxide-export",
  "format_version": 1,
  "exported_at": 1790000600000,
  "account": {
    "user_id": "0199a3c4-…",
    "created_at": 1789000000000,
    "plan": "free",
    "display_name": "Jules"
  },
  "sign_in": {
    "passkeys": [
      { "nickname": "iPhone", "created_at": 1789000000000, "last_used_at": 1790000000000, "backed_up": true }
    ],
    "linked_accounts": [
      { "provider": "google", "created_at": 1789000100000, "last_used_at": null }
    ]
  },
  "settings": {
    "unit": "kg", "bar_weight": 20.0,
    "plate_inventory": [{ "plate": 20.0, "pairs": 4 }, { "plate": 2.5, "pairs": 2 }],
    "default_rest": 120, "sound_enabled": true,
    "updated_at": 1789500000000
  },
  "training_maxes": [
    { "exercise_id": "bench-press", "weight": 82.5, "set_at": 1789600000000 }
  ],
  "programs": [
    {
      "creation_id": "0199a3c5-…",
      "name": "Full body",
      "source_builtin_id": "full-body-3day",
      "archived": false,
      "created_at": 1789000200000,
      "versions": [
        { "version": 1, "created_at": 1789000200000, "document": { "schema_version": 1, "name": "Full body", "…": "…" } }
      ]
    }
  ],
  "active_program": "0199a3c5-…",
  "sessions": [
    {
      "id": "0199a3c6-…",
      "program": "0199a3c5-…",
      "version": 1,
      "day": "a",
      "status": "completed",
      "started_at": 1790000000000,
      "finished_at": 1790003600000,
      "sets": [
        { "id": "0199a3c7-…", "exercise": "back-squat", "set_index": 0, "reps": 5, "weight": 100.0,
          "duration": null, "warm_up": false, "completed_at": 1790000300000 }
      ]
    }
  ]
}
```

## Fields

The values use the API's types (`docs/api.md`):

- **Times** are milliseconds since the Unix epoch, UTC. Server-side times (`created_at`, `set_at`,
  `updated_at`) are stored in microseconds, and their sub-millisecond digits are dropped.
- **Weights** are kg numbers (the domain `Weight`).
- `default_rest` and `duration` are seconds.
- **Ids** are UUIDs and slugs.

| Field | What | On import |
|---|---|---|
| `format` | Always `iron-oxide-export` | Checked first: anything else is `422 This file is not an Iron Oxide export.` |
| `format_version` | `1` | Checked before the rest is parsed: another version is a `422` that names it |
| `exported_at` | When the export was made | Ignored |
| `account` | User id, creation time, plan, display name | **Ignored.** An import never changes the account, its plan or its name. |
| `sign_in.passkeys` | Each passkey's nickname, dates and whether it is synced (`backed_up`). No public key, credential id or user handle. | Ignored: a passkey cannot be restored from a file |
| `sign_in.linked_accounts` | Linked providers (`google`) and dates. No provider subject. | Ignored |
| `settings` | The saved settings (as `get_settings`) and `updated_at`; `null` if never saved | Validated like `update_settings`; added only if the account has no settings |
| `training_maxes` | One per exercise | Weight more than zero, one per exercise; added for exercises without one |
| `programs` | The user's programs, archived ones too, oldest first, each with **all** its versions | See below |
| `programs[].creation_id` | The program's key (the client idempotency key it was created with, unique per user) | Matches an existing program |
| `programs[].versions[].document` | The `program.json` as stored | Must pass the domain's `Program::from_json` (`422` with the problems, their paths starting at `programs[i].versions[j].document`) |
| `active_program` | The `creation_id` of the active program, or `null` | Must be an unarchived program of the export; set only if the account has no active program |
| `sessions` | Every workout session, oldest first | See below |
| `sessions[].program`, `.version` | The session's program (`creation_id`) and version number | Must be a version in the export, and `day` one of its days |
| `sessions[].sets` | The session's sets, in the order they were completed (the domain `LoggedSet`) | Set ids unique in the file |

Server-generated ids (program and version ids) are not in the export: they are global, so another
account could not reuse them. Programs are keyed by `creation_id`, versions by their number, and
sessions point at both. Session and set ids are kept, because they are only unique per user.

Not exported:

- sign-in sessions (`sessions`), the WebAuthn user handle and in-flight sign-in ceremonies;
- built-in programs (only the user's copies);
- other users' data, by construction: every query is scoped to the signed-in user.

`every_user_table_is_exported_or_deliberately_left_out` lists every table with a `user_id` column
and where it goes. A new table fails the test until it is added there and here.

## Import rules

1. **Size.** The request body is capped at `IMPORT_BODY_LIMIT` (2 × `MAX_EXPORT_BYTES` + 64 KiB,
   since the file travels as a JSON string). The session is checked before the body is read, so a
   signed-out request gets `401` without uploading anything. The document itself is capped at
   `MAX_EXPORT_BYTES` (8 MiB). Both give `413`.
2. **Version, then content.** All of the document is validated before anything is written:
   - the domain types: slugs, weights, reps;
   - the domain validators: program documents, settings;
   - the references inside the file: a session's version and day, the active program;
   - uniqueness: creation ids, version numbers, session and set ids, at most one session in
     progress;
   - the session rules: an end time if and only if the session ended, and not before its start.

   Any failure is `422` and nothing is written. Sets are **not** checked against their session's
   time span: stored sets may fall outside it (see `docs/api.md`), and every export must import
   back.
3. **One transaction.** It starts by locking the user's row, like every quota write
   (`docs/billing.md`), so concurrent imports of one user run one after the other.
4. **Quota: refused, never trimmed.**
   - New unarchived programs count toward the plan's `CustomPrograms` limit. Programs the account
     already has, and archived ones, take no slot.
   - If the new unarchived programs do not all fit, the import is refused with `403` ("Your plan
     keeps up to 10 programs. Archive one, or upgrade to Pro.") before anything is written.
   - Each new unarchived program still goes through `reserve_quota`.
   - Re-importing an export into an account at its limit adds nothing, so it is never refused.
5. **Conflicts: what the account already has wins.** An import only inserts rows the account does
   not have. It never updates or deletes one:

   | Row | Matched by | If the account has it |
   |---|---|---|
   | Settings | the user | kept |
   | Training max | exercise id | kept |
   | Program | `creation_id` | kept (name, archived flag), and gets the export's versions it does not have |
   | Program version | program and version number | kept: the export's sessions of that version then refer to the account's version |
   | Active program | the user | kept |
   | Session | session id | kept, with its own sets (the export's sets of that session are not added) |
   | Session in progress | at most one per user | if the account already has another one in progress, the export's one is skipped with its sets |
   | Set | set id | kept |

   Programs and versions get new ids. Every other id is kept.
6. **Idempotent.** Importing the same export a second time finds every row already there and adds
   nothing (`ImportSummary` all zero).

The answer is an `ImportSummary`: whether settings and the active program were set, and how many
training maxes, programs, versions, sessions and sets were added.

## Export size

An export larger than `MAX_EXPORT_BYTES` (8 MiB of compact JSON, about 40,000 logged sets) is
refused with `413` and a message to contact support. That keeps every export importable. At three
sessions a week it takes years to reach.

## Versioning

`format_version` changes when a reader of the previous version would misread a document:
- a field is removed or renamed, or its meaning or unit changes;
- a field is added that an import must not ignore.

A new optional field that an older reader can safely ignore does not need a new version. Unknown
fields are ignored on import.

When the version changes, the server keeps reading the versions it can still convert, and this page
documents each one.

## Account deletion

`delete_account()` (`POST /api/account/delete`) deletes the account and everything above:

- **A fresh sign-in is the confirmation.** The session must have signed in within the last 10
  minutes (`DELETE_REAUTH_WINDOW_SECS`), otherwise `403 To delete your account, sign in again
  first.` The UI then asks for a passkey or Google sign-in and retries.
  - A stolen session cookie, or a page left open, cannot delete the account.
  - The CSRF check refuses cross-site requests.
- **Billing first.** `server::billing::cancel_before_account_deletion` runs before anything is
  deleted, and its failure stops the deletion.
  - Today it is a no-op stub: Stripe is not implemented yet. `docs/billing.md` describes what it
    must do (cancel the subscription immediately).
- **One statement.** `DELETE FROM users` runs in one transaction and cascades to every user-owned
  table. Every sign-in session goes too, so every device is signed out at once.
  - Programs and versions refuse any direct `DELETE` (`forbid_direct_delete`), so this cascade is
    the only way they are ever deleted.
- **Signed out.** The answer clears the cookie. A replayed request has no session left and gets
  `401`.
