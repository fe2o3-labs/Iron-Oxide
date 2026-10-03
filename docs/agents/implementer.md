# Implementer rules

Every implementer brief points here. Follow it entirely, together with [CLAUDE.md](../../CLAUDE.md) and
[CONTRIBUTING.md](../../CONTRIBUTING.md).

## Workflow

- Read your ticket first: `gh issue view N -R fe2o3-labs/Iron-Oxide --comments`, and the docs it points to.
  Always pass `-R fe2o3-labs/Iron-Oxide` and explicit PR/issue numbers to `gh`.
- Work **only in your own git worktree** (`git worktree add <scratch>/wt/<branch> -b <branch> origin/main`, or
  from the base branch named in the brief). Use **`git -C <worktree>`** for every git command. Never touch the
  main checkout, never commit to `main`.
- **Never `git stash`** (shared by all worktrees). **Never `pkill`/`killall` by name**: kill only PIDs you
  started. Temporary files (PR bodies, logs, probes) go in a folder named after your branch.
- One ticket → one branch → one PR, opened ready for review, body `Closes #N`. Stacked work: branch from the
  named base and `gh pr create --base <base branch>`.
- **No rebase, no force-push.** When the base moves, merge it in. git rerere is disabled for this repo; resolve
  conflicts by hand. Conflicts on files of an already squash-merged PR: take `main`'s version unless you changed
  them on purpose. Afterwards `git diff origin/main...HEAD` must show only your ticket.
- **No attribution anywhere** (commits, PR text, comments, code, docs). Commit with the configured git identity.
- **Latest stable versions** of anything you add or bump (check crates.io / release pages live). Hold one back
  only when another dependency forces it, and say why in a comment.
- New tooling gets a `make` target. Use the `make` targets, not raw commands, when one exists.
- Do only your ticket. Note follow-ups in your report instead of widening the scope.

## Checks

- Code changes: `make fmt`, `make lint`, `make test`, and `make test-db` when anything touches Postgres.
  Queries changed → `make sqlx-prepare` and commit `.sqlx/`. Program types changed → `make schema`.
- **Pure merge of `main`** (no code of your own): `make compile` (or `cargo check --workspace --all-targets` and
  with `--features server`), push, and let CI gate it.
- After pushing: `gh pr checks N --watch` until green; the 7 required checks are gitleaks, rustfmt, clippy, unit
  tests, Postgres integration tests, sqlx offline build, `dx bundle` + smoke test.
- **Regression tests must fail without the fix**: revert the fix locally, run the test, see it fail, restore.
- UI: check in a real browser (Playwright or Chrome DevTools tools) against `make dev` at 390 × 844 in both
  themes, in your own browser context and on your own dev-server port; sign in with the browser's virtual
  authenticator (passkey). Save screenshots in your scratch folder and list them in a PR comment (`gh` can't
  upload images; the maintainer drags them in).

## Machine

- macOS. If git complains about the Xcode licence: `export DEVELOPER_DIR=/Library/Developer/CommandLineTools`.
  Use rustup's `cargo` (`$HOME/.cargo/bin` first in `PATH`; Homebrew's ignores `rust-toolchain.toml`).
- `export CARGO_INCREMENTAL=0` for long-lived build dirs.
- **Your own `CARGO_TARGET_DIR`** per branch (a shared one serves stale binaries), reused across updates of that
  branch, deleted when the PR is merged. `sqlx prepare` and reviewers use a private one too.
- **Your own Postgres:** `unset DATABASE_URL SESSION_KEY`; use your own compose project and port
  (`COMPOSE_PROJECT=… PG_PORT=…` for the `make` targets). If Docker (Colima) is down, a temporary Homebrew
  Postgres 18 on a private port is fine; stop and delete it afterwards.
- Check `df -h /` before big builds; stop and report if under ~12 GB free.
- Long commands run in the background; don't leave stray processes.

## Reporting

Reply to the coordinator in the number of lines the brief asks for (usually ≤ 8): PR number, what's in, what's
not, CI status, decisions beyond the ticket, anything the maintainer must decide. When done, remove your
worktree and private dirs (keep them if the brief says more work is coming).

## When the budget is tight

Finish only your task, no sub-agents, no extra scope, short reports. Reviewers answer only the brief's
questions.
