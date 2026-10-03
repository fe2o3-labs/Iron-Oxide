# Independent reviewer

A fresh agent, with no context from the session that produced the PR, reviews one PR (or one fix round) for
**correctness only**. Follow [implementer.md](implementer.md) for the machine and safety rules (own detached
worktree, `git -C`, private target dir, own `.env`, Postgres and `APP_PORT`, no stash, no pkill), and delete
everything you created when done.

## Scope

- Defects, broken contracts and invariants, races, edge cases, data isolation between users, security
  (auth, CSRF, step-up, limits), data loss, idempotency of writes, and the tests that would have caught them.
- **Not** style, wording, naming or performance opinions (unless the PR's purpose is a bound: then measure it).
- Read GitHub Copilot's inline comments on the PR
  (`gh api repos/fe2o3-labs/Iron-Oxide/pulls/N/comments`, author login containing "copilot") and say for each
  whether it is a real defect.
- Answer the brief's questions; report adjacent bugs outside the PR as notes (the coordinator files tickets).

## Method

- **Prove every finding** with a test you ran, a request against `make dev`, or a real browser run (Playwright
  or Chrome DevTools tools, virtual passkey, 390 × 844, both themes for UI). Reasoning-only findings are
  labelled as such.
- Probe against `origin/main` in a second worktree when a behaviour change is claimed.
- Mutation check: revert or break the code under test; the PR's tests must fail.
- For a **fix round**, check only the fix commits: each previous finding is fixed, no new defect, the regression
  test fails without the fix.
- Don't push your probe tests; describe them in the review so the implementer can add them.

## Deliverable

One review per PR: write the body to a file in your own folder, then
`gh pr review N -R fe2o3-labs/Iron-Oxide --comment --body-file <file>`. First line exactly
**`Verdict: no correctness defect found`** or **`Verdict: correctness defect(s) found`**, then what was checked
and how, then each finding with `file:line` and a reproducer, then non-blocking nits separately. No attribution
(see CLAUDE.md, rule 2). Report back to the coordinator in the number of lines asked.
