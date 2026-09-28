# Backups and point-in-time restore

Runbook for recovering the Iron Oxide database. It covers what Neon keeps, how to restore it, and how to rehearse the restore.

Setup (decisions #38 and #39): Postgres on **Neon, AWS Frankfurt (`eu-central-1`)**, app on **Fly.io, region `fra`**. The app connects through the **direct (non-pooled) endpoint**.

Placeholders used below:

| Placeholder     | Meaning                                                                                                      |
| --------------- | ------------------------------------------------------------------------------------------------------------ |
| `<app>`         | Fly app name (not chosen yet)                                                                                |
| `<prod-branch>` | Neon root branch the app uses (`production` if the project was created in the Console, `main` via CLI/API)   |
| `<project-id>`  | Neon project ID (Console, **Settings**), or run `neon set-context --project-id <project-id>` once            |
| `<ts>`          | Restore point, RFC 3339 in **UTC**, e.g. `2026-09-28T12:05:00Z`                                              |

## 1. What Neon keeps

Neon doesn't take nightly backups. It keeps the write-ahead log (WAL) for a **history window**, and you can restore a root branch to any moment inside that window, down to the millisecond ("instant restore", i.e. PITR).

As of 2026-09-28:

| Plan   | Default history window | Maximum                   | History storage cost |
| ------ | ---------------------- | ------------------------- | -------------------- |
| Free   | 6 hours                | 6 hours (capped at 1 GB)  | free                 |
| Launch | 1 day                  | 7 days                    | $0.20 / GB-month     |
| Scale  | 1 day                  | 30 days                   | $0.20 / GB-month     |

Sources (read 2026-09-28):

- History window: <https://neon.com/docs/postgres/backup-restore/history-window>
- Plans: <https://neon.com/docs/introduction/plans>
- Instant restore: <https://neon.com/docs/postgres/backup-restore/branch-restore>

Things to know:

- **Only root branches** (e.g. `<prod-branch>`) can be restored to a point in time. Child branches cannot.
- A restore covers **all databases on the branch**. It **overwrites** them and does not merge.
- Object Storage and Neon Functions are not part of the timeline, so a restore doesn't touch them.
- On Free, 6 hours means a mistake noticed the next morning **cannot be undone**.

**Recommendation:** before any real user data matters, move to **Launch** and set the history window to **7 days**. Upgrading alone leaves it at the 1-day default. Set it in the Console under **Settings → Postgres → History window**: move the slider to 7 days, then **Save**. The cost is the retained WAL at $0.20/GB-month, which is tiny for this app.

## 2. Before you restore

1. **Record what to come back to**, whether or not you stop the app:
   - `<image-ref>`: the image of the last known-good release, from `fly releases --image -a <app>`.
   - `<good-sha>`: the git commit that release was built from, from the deploy's CI run or `git log`.

   ```bash
   fly releases --image -a <app>
   ```

   **Stop the bleeding.** If the app is still doing damage (a bad deploy or a runaway job), stop it:

   ```bash
   fly scale count 0 -a <app>
   ```

   Scaling to 0 **destroys** the Machines, so `fly scale count 1`, `fly apps restart` and `fly secrets deploy` can no longer start anything. The only way back is `fly deploy` ([Fly docs](https://fly.io/docs/apps/scale-count/)).

   **"Start the app"** (referenced below) means bringing the app back after the restore:

   - **If you scaled to 0:** run the command below from a checkout of the repo, so `fly.toml` is picked up. A deploy from zero recreates the Machines from `fly.toml`: two started Machines for each process group with services, one started plus one stopped standby for a group without ([Fly docs](https://fly.io/docs/apps/app-availability/)).

     ```bash
     fly deploy --image <image-ref> -a <app>   # known-good release, or the fixed release
     ```

     A bare `fly deploy` would build whatever is in your working tree, which may be the bad release. Fly may prune images that haven't been deployed for a while. If the image can't be pulled, rebuild the same code instead: `git checkout <good-sha>` (or the fixed commit), then run `fly deploy -a <app>` from there.
   - **If the app is still running:** run `fly apps restart <app>`, so it opens fresh connections. If you staged a secret, run `fly secrets deploy -a <app>` instead: it redeploys the current release with the staged secrets.

2. **Find the restore point in UTC.** Neon takes RFC 3339 timestamps, and `Z` means UTC. Paris is UTC+2 in summer and UTC+1 in winter.
   - `fly logs -a <app>` and `fly releases -a <app>`: look for the bad deploy or request. Check the timestamps' zone (a `Z` suffix means UTC) before copying them.
   - Look at `created_at`/`updated_at` columns near the incident.
   - Use **Time Travel** to query the past read-only, and bisect until you find the last good moment. You can do this from the Console (**SQL Editor**, clock icon) or from the CLI:

     ```bash
     neon connection-string <prod-branch>@2026-09-28T12:05:00Z --psql -- -c "select count(*) from <table>"
     ```

   - Convert a local time to UTC:

     ```bash
     # macOS (BSD date)
     date -u -r "$(TZ=Europe/Paris date -j -f '%Y-%m-%d %H:%M:%S' '2026-09-28 14:05:00' +%s)" +%Y-%m-%dT%H:%M:%SZ
     # GNU date (Linux, or `gdate` from Homebrew coreutils)
     date -u -d 'TZ="Europe/Paris" 2026-09-28 14:05' +%Y-%m-%dT%H:%M:%SZ
     # both print 2026-09-28T12:05:00Z
     ```

   Pick a time **just before** the bad change. Everything written after `<ts>` is dropped from the restored branch, though it stays in the backup branch.

3. **Think about migrations.** Migrations run automatically when the app starts.
   - If you restore to a point **before a migration**, the database's migration table goes back too. The next boot **re-runs that migration**.
   - If the incident *was* a bad migration, restoring and then booting the same image repeats the damage. Keep the app scaled to 0 until a fixed release exists. Then "start the app" with that release's image, never with the bad one.
   - Rolling the **code** back to an older release while the database keeps a newer schema can make the migrator refuse to start, because it sees "applied migration missing from source". Check how the migrator is configured before relying on a code rollback.

## 3. Restore

There are two paths:

- **A. Inspect first** (recommended when unsure): restore into a new branch, check it, then restore production in place.
- **B. In place**: restore `<prod-branch>` directly. The old state is kept automatically as a backup branch.

The connection string **does not change** with an in-place restore, because Neon moves the compute over to the restored branch. You only change `DATABASE_URL` in the fallback at the end of path A.

### A. Restore into a new branch and inspect

**Console:** **Branches** → **New branch**. Set **Parent branch** to `<prod-branch>` and choose **Past data**. Pick the date and time, name the branch `restore-check`, and create it.

**CLI** (install with `npm i -g neon@latest` or `brew install neonctl`, then run `neon login`; syntax checked against `neon` 6.2.3):

```bash
# --parent accepts a timestamp; the parent is then the project's default branch (<prod-branch>)
neon branches create --name restore-check --parent 2026-09-28T12:05:00Z
```

**Verify** (see [section 4](#4-verify)) with:

```bash
neon connection-string restore-check --psql
```

Once the data looks right, apply the same `<ts>` to production **in place (path B)**. Production then stays a root branch, keeps PITR, and keeps its connection string. Afterwards, delete `restore-check`:

```bash
neon branches delete restore-check
```

If only a few rows were lost and you want to keep the writes made since, don't restore at all. Copy the missing rows from `restore-check` into `<prod-branch>` instead, with `pg_dump --data-only -t <table>` or `\copy`.

**Fallback: run the app on the new branch.** Use this only if an in-place restore is impossible. `restore-check` is a **child branch**, so while production runs on it you **lose point-in-time restore**. Treat it as temporary.

```bash
# Direct endpoint: never pass --pooled (decision #39). $(...) keeps the password out of your shell history.
# The && chain and ${url:?} stop everything if neon fails or prints nothing, so an empty DATABASE_URL is never staged.
# --stage: store the secret without deploying (there may be no Machines to roll if you scaled to 0).
url="$(neon connection-string restore-check)" \
  && fly secrets set --stage DATABASE_URL="${url:?empty connection string}" -a <app>
```

Then **start the app** ([step 2.1](#2-before-you-restore)). If you scaled to 0, run `fly deploy --image <image-ref> -a <app>`; the new Machines pick up the staged secret. If the app is still running, run `fly secrets deploy -a <app>`.

### B. Restore production in place

**Console:**

1. Select `<prod-branch>` and open **Postgres database → Backup & Restore → Restore from history**.
2. Pick the timestamp, or switch to LSN. Use the built-in **Time Travel Assist** query box to check the data at that point.
3. Click **Next**, review the summary, then click **Restore**.

**CLI:**

```bash
neon branches restore <prod-branch> ^self@2026-09-28T12:05:00Z \
  --preserve-under-name <prod-branch>_before_restore_20260928
```

`--preserve-under-name` is mandatory for `^self`. The pre-restore state is kept as a root branch with that name (the Console names it `<prod-branch>_old_<timestamp>`).

- Open connections drop for a few seconds during the restore. The restore itself takes seconds.
- **Start the app** as described in [step 2.1](#2-before-you-restore): `fly deploy --image <image-ref> -a <app>` if you scaled to 0, otherwise `fly apps restart <app>`.
- **Undo the restore:** restore `<prod-branch>` again, using the backup branch as the source. **Always pass `--preserve-under-name`.** Anything written since the restore exists only in `<prod-branch>`, and the flag keeps it in a new backup branch. Neon only guarantees that backup when the flag is given. Stop the app first (step 2.1), then start it again afterwards.

  ```bash
  neon branches restore <prod-branch> <prod-branch>_before_restore_20260928 \
    --preserve-under-name <prod-branch>_before_undo_20260928
  ```

  Per the Neon docs, the branch preserved when a root branch is restored *from another branch* **cannot be deleted**. You can only drop its tables to reclaim storage.

## 4. Verify

Run these checks against the restored branch (`restore-check`, or `<prod-branch>` after path B):

- [ ] The data you expected to recover is back. Spot-check the affected user/table with the query you used to find `<ts>`.
- [ ] Nothing newer than `<ts>` is present, e.g. `select max(created_at) from <table>` is `<= <ts>`.
- [ ] `select version, description, success from _sqlx_migrations order by version desc limit 5;` shows the migration level you expect (see [migrations](#2-before-you-restore)).
- [ ] The app boots. `fly logs -a <app>` shows migrations applied (or none pending) and no errors.
- [ ] Log in and exercise the main flows end to end (read existing data, write something new).
- [ ] Once confident, delete the backup branch (Console → **Branches**) to stop paying for its storage. Some backup branches cannot be deleted; see the instant-restore docs.

## 5. Restore drill

Do this once before real data matters, then after any change to the database setup. Use a throwaway timestamp; it's safe because path A doesn't touch production.

- [ ] Note the plan and current history window (Console → **Settings → Postgres**).
- [ ] Pick `<ts>` about 1 hour ago and convert it to UTC.
- [ ] Create `restore-check` from `<ts>` (path A) and connect with `psql`.
- [ ] Verify a row written after `<ts>` is absent and an older row is present.
- [ ] Optional, on a non-production project or branch: do an in-place restore (path B) and then undo it.
- [ ] Clean up. Delete `restore-check` first: after a `^self` restore it is moved under the backup branch, and a branch with children can't be deleted. Then delete the `_before_restore_*` backup branch. The `_before_undo_*` branch from an undo **cannot be deleted** (Neon keeps the original root when a root branch is restored from another branch), so connect to it and drop its tables to free the storage.
- [ ] Log the drill below.

| Date (UTC) | Who | Plan / history window | Path tested | Time to restored & verified | Notes |
| ---------- | --- | --------------------- | ----------- | --------------------------- | ----- |
|            |     |                       |             |                             |       |

## 6. Extra: logical backup with `pg_dump`

PITR only reaches as far back as the history window, and it lives inside Neon. For an off-Neon copy (before risky changes, before leaving Neon, or for long-term archiving), take a logical dump.

- Use the direct endpoint only: `pg_dump` over the pooler is not supported.
- `pg_dump` refuses to dump a server with a newer major version, so its major version must be **the same as or newer than** the server's (`show server_version;`).
- `pg_restore` must be at least as new as the `pg_dump` that wrote the archive.

```bash
# The && chain and ${url:?} stop if neon fails or prints nothing (an empty -d would target a local database).
url="$(neon connection-string <prod-branch>)" \
  && pg_dump -Fc -v -d "${url:?empty connection string}" \
       -f ~/iron-oxide-backups/iron-oxide-$(date -u +%Y%m%dT%H%M%SZ).dump
```

The restore target must not already contain the application schema. A new Neon branch is a **copy of its parent**, not an empty database, so `pg_restore` into a branch of `<prod-branch>` would fail on existing objects or duplicate rows. Restore into a **freshly created database** instead, e.g. on a scratch branch:

```bash
neon branches create --name dump-check
neon databases create --branch dump-check --name restore_target
url="$(neon connection-string dump-check --database-name restore_target)" \
  && pg_restore -v --no-owner -d "${url:?empty connection string}" ~/iron-oxide-backups/<file>.dump
```

> **Never commit dumps.** They contain user data (accounts, emails, workout history) and this repository is **public**. Write them outside the repo, keep them encrypted, and delete old ones.

Docs: <https://neon.com/docs/manage/backup-pg-dump> (read 2026-09-28).
