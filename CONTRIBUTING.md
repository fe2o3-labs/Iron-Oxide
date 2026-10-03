# Contributing

How work gets done in this repository. The [README](README.md) covers setup, configuration and the `make`
targets; this file covers the process.

## Tickets and pull requests

- **One ticket, one branch, one pull request.** Every change starts from an issue; the PR body says
  `Closes #N`. Even a chore gets a (short) ticket.
- The plan lives in two pinned issues: the **tracker** of the current version (#40) and the **roadmap** (#86).
  Design decisions are logged in **#41**, with the date and the PR that implements them.
- Branch from `origin/main`, named `feat/<ticket>-<topic>`, `fix/<ticket>-<topic>`, `chore/…` or `docs/…`.
  Work in your own [git worktree](https://git-scm.com/docs/git-worktree) when several branches are in flight.
- **Nothing is committed to `main` directly.** The `main` branch is protected: the required checks must pass,
  and PRs are **squash-merged** by the maintainer.
- A PR that builds on another, unmerged PR targets that PR's branch (a *stacked* PR). When the base is merged,
  GitHub retargets it to `main`.

## Keeping a branch up to date

- **Merge `origin/main` into your branch; don't rebase or force-push** a branch that has a PR. Reviews and
  comments stay attached to the commits.
- Because `main` is squash-merged, a branch that contains the commits of an already merged PR conflicts on
  that PR's files. Resolve those conflicts with **`main`'s version** (it is the merged PR's final state), unless
  your branch changed the file on purpose.
- After resolving, `git diff origin/main...HEAD` must show **only your ticket's changes**.
- **A pure merge** (no code change of your own) only needs `make compile` locally before pushing: CI is the
  gate. A branch with no conflict can simply be updated with GitHub's *Update branch* button
  (`gh pr update-branch N`).
- After a PR is merged, update the other open PRs from `main` straight away, so they don't sit outdated.

## Checks

- Before pushing code changes: `make fmt`, `make lint`, `make test`, and `make test-db` when you touched
  anything that talks to Postgres. `make check` runs everything CI runs, in CI's order.
- After pushing, watch CI (`gh pr checks N --watch`) until it is green. The required checks are: secret scan
  (gitleaks), rustfmt, clippy, unit tests, Postgres integration tests, sqlx offline build, and the
  `dx bundle` + smoke test.
- **sqlx:** queries use the compile-time macros with the committed `.sqlx/`. After changing a query, run
  `make sqlx-prepare` and commit `.sqlx/`.
- **Program schema:** after changing the program types, run `make schema` and commit
  `schemas/program.schema.json`.
- **UI changes:** check them in a real browser (`make dev`, <http://localhost:8080>) at phone size
  (390 × 844) in **both** the dark and the light theme, and attach screenshots to the PR.

## Reviews

Every PR gets:

1. A **check against the ticket**: does it do what the ticket asks, with tests, and nothing else?
2. An **independent correctness review** by someone who didn't write it, posted as one PR review whose first
   line is `Verdict: no correctness defect found` or `Verdict: correctness defect(s) found`, followed by each
   finding with `file:line` and a way to reproduce it.
3. A triage of **GitHub Copilot's review comments**, if Copilot reviewed the PR: each one is fixed, moved to a
   follow-up ticket, or rejected with the reason.

Findings are fixed in **new commits** on the same branch, each with a regression test that **fails without the
fix** (check it by reverting the fix locally). A fix round gets a narrow re-check of just those commits. When
the findings of successive rounds keep shrinking, the last round can be checked by the ticket check alone.

Anything found but out of scope becomes a new ticket, linked from the PR.

## Data and compatibility rules

- **Tightening a validation rule ships with a migration** that fixes the data already stored, so stored data,
  and users' own exports, keep loading.
- Every write is **idempotent by a client-generated id** (UUIDv7): replaying it changes nothing. See
  [docs/api.md](docs/api.md).
- Every table that holds user data follows the isolation rules in [docs/database.md](docs/database.md), and its
  endpoints have an isolation test.

## Local environment

- `make setup` installs the pinned toolchain, `dx` and `sqlx-cli`; `make versions` shows what's installed.
- Several builds at once (worktrees) should each use their own `CARGO_TARGET_DIR`: a shared one can serve one
  worktree's stale binaries to another. Reuse the same directory across updates of one branch so builds stay
  warm, and delete it when the branch is merged. `make prune` trims old build artefacts.
- A worktree must not inherit the main checkout's `DATABASE_URL` or `SESSION_KEY`; use a separate compose
  project and port (`COMPOSE_PROJECT`, `PG_PORT`) for its database.
- Keep an eye on free disk space during large builds.

## Dependencies

- Use the **latest stable** version of crates, tools, GitHub Actions and images, checked at the time you add or
  bump them. [Renovate](renovate.json) keeps them current afterwards.
- New tooling gets a `make` target.

## Secrets and privacy

- This repository is **public**. Never commit secrets, credentials or real `.env` files; `.env*.example` files
  hold placeholders only. CI scans every PR with gitleaks.
- Report security issues as described in [SECURITY.md](SECURITY.md), not in public issues.

## Licence

By contributing, you agree that your contributions are licensed under the [AGPL-3.0](LICENSE).
