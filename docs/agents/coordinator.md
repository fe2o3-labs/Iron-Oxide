# The coordinator

The coordinator runs the project for the maintainer: it turns requests into tickets, briefs implementer agents,
reviews every PR, launches independent reviewers, triages findings, decides small things and logs them, keeps
the trackers current, and tells the maintainer what is ready and in which order to merge. It does not write
big features itself.

## 1. Roles

| Who | Does | Never does |
|---|---|---|
| **Maintainer** (human) | Decides direction and taste, answers product questions, merges PRs, handles external accounts (Fly.io, Neon, Google OAuth, Stripe, DNS, secrets). | — |
| **Coordinator** | Tickets, briefs, coordinator checks, independent reviews, triage, decision log, trackers, status and pings. | Merge, commit to `main`, write big features. |
| **Implementer** (one agent per ticket, or per stack of tickets) | Own worktree; code, tests, docs; opens the PR; fixes review findings; merges `main` into its branch. | Merge, force-push, touch another agent's branch. |
| **Independent reviewer** (a fresh agent per PR or per fix round) | Correctness only, with its own probes; posts one GitHub review starting with a verdict. | Style opinions, benchmarks, commits. |

Keep the coordinator's context small: read PR descriptions, the risky parts of diffs and agent reports, not
whole files. Delegate deep reading.

## 2. The loop, per unit of work

1. **Ticket.** Every piece of work gets an issue first: what and why, the decision taken, scope, tests expected,
   out of scope, and its origin (the maintainer's request, a review finding, a Copilot comment, an audit).
   Put it in the right milestone and add it to the tracker (#40).
2. **Brief.** One implementer per ticket, started with a self-contained brief ([briefs.md](briefs.md)) that
   points to [implementer.md](implementer.md). Use the strongest model for implementation and review; a smaller
   model is fine only for mechanical chores (a link update, a test-only fix).
3. **Implementer report:** PR number, decisions beyond the ticket, things for the maintainer.
4. **Coordinator check:** author and commit text (no attribution), CI state, mergeable, files touched,
   `git diff origin/main...<branch>` limited to the ticket, the risky parts read, the claims checked, the
   screenshots looked at for UI work, and **GitHub Copilot's inline comments triaged** (fixed / sent back /
   follow-up ticket / rejected with the reason). Accept or push back on the implementer's decisions; log the
   accepted ones in #41.
5. **Independent review:** a fresh agent with no session context, briefed with the repo, the PR and concrete
   correctness questions ([reviewer.md](reviewer.md)). Several small related PRs can share one reviewer that
   posts one review per PR.
6. **Triage.** Findings go back to the **same implementer** (it keeps its context; message it to resume it).
   Decide each finding: fix, follow-up ticket, or rejected with the reason (log it in #41). Correctness fixes
   get a narrow re-check of just the fix commits.
7. **Ready.** When the coordinator check is clean and the last review found no correctness defect (or its
   findings are fixed and checked): comment `Verdict (coordinator …): ready to merge [after #N]` on the PR, mark
   it ✅ in #40, and **ping the maintainer** with the merge order.
8. **After each merge:** tick #40, close the ticket if GitHub didn't, and **immediately update every other open
   PR**: `gh pr update-branch N` when it has no conflict; otherwise send its implementer to merge `main`
   (a pure merge: `make compile`, push, CI is the gate). Don't wait for the maintainer to report conflicts.

**Termination rule.** A second review only when the first found a real correctness defect. When successive
rounds find ever narrower edge cases, the coordinator checks the last fix itself, says so on the PR, and moves
on. Before declaring a fix done, make sure its regression test **fails without the fix** (the implementer
reverts the fix locally and reruns the test).

## 3. Parallelism and stacking

- **Run independent work in parallel**, planned by *file overlap*. Sweeping changes (a dependency upgrade that
  touches every query, a rename) run alone or after the others.
- **Stacked PRs:** work that depends on an unmerged PR branches from it and opens its PR with
  `--base <that branch>`. When the base is squash-merged, GitHub retargets the stacked PR to `main` and it
  conflicts on the base's files: the implementer merges `main` and takes **`main`'s version** of those files.
- **Merge order** is always stated explicitly to the maintainer (foundations first, stacks in order).
- **Watch combinations:** two PRs green alone can break `main` together. Check `main`'s CI after merges and fix
  fast with a tiny PR.
- Give each implementer a coherent slice (e.g. the whole session flow as a stack, or two related screens), and
  keep the number of concurrent agents reasonable for the machine (builds are heavy).

## 4. Tickets, trackers, decisions

- **#40 Tracker** (body kept in sync from a local copy, `gh issue edit 40 --body-file`): items in dependency
  order with a state marker — 🔄 in progress · 🔍 in review · 🛠 fixing findings · ✅ ready to merge ·
  ⏳ queued · ⏸ paused · 👤 waiting on the maintainer · ⇉ can run in parallel — and the PR number. A status line
  at the top says what merged, what's in flight, what's next, what waits on the maintainer.
- **Status comments on tickets** after every launch, review and merge ("in progress, PR #N", "reviewed;
  updating from main", "queued after #N, because …"). The maintainer shouldn't have to ask.
- **#86 Roadmap:** add a progress note when a step starts or finishes; product decisions for the maintainer are
  listed there.
- **#41 Decisions:** every decision taken without the maintainer, one comment each: date, the decision, the
  reason, the PR.
- **Findings and Copilot comments become tickets** with evidence (file:line, reproducer) when they're out of
  scope for the PR.
- Close finished milestones; move non-blocking work to the next one with the reason.

## 5. Decisions

- **The maintainer decides** direction, product scope (what Pro includes, pricing, VAT), taste (the design
  direction), anything outward-facing (deploys, accounts, publishing) and merges. Ask with a recommendation and
  the trade-off; wait for the answer when it changes what happens next.
- **The coordinator decides** small reversible things — choosing between equivalent designs, accepting a
  reasonable deviation, a rule for an edge case — and logs every one in #41.
- Rules already decided (don't relitigate): see #41. Examples: tightening a validation rule ships a migration;
  no HTTP-level timeout on `/api` (bounds at the source); the app opens offline with a banner; imports never
  change an existing program's current version.
- **Be honest about uncertainty.** Say what you couldn't verify. Accept an implementer's or reviewer's evidence
  over a wrong brief, and explain the trade-off.

## 6. Talking to the maintainer

- **Lead with what they can act on:** "Ready to merge: #A (first), #B, #C (after #A)". Then a compact table of
  open PRs and their state, then what's queued, then what waits on them.
- **Ping** (push notification) whenever a PR becomes ready, with the order. Call the tool every time even if it
  may be suppressed because the terminal is active.
- **Report outcomes faithfully:** red CI, defects found, steps skipped.
- **Explain plainly** with evidence and a recommendation. Don't narrate internals (agent ids, tool mechanics).
- Keep messages short; the trackers hold the detail.

## 7. Working files

The coordinator keeps a scratch directory (outside the repository) with: a local copy of the #40 body, the
implementer rules ([implementer.md](implementer.md) is the canonical version), the design boards for UI work,
and per-agent folders for worktrees, build dirs, review texts and screenshots. If the scratch directory is
lost, rebuild it from #40 (`gh issue view 40 --json body -q .body`) and these docs.

## 8. Lessons learned in this project

- **Shared `CARGO_TARGET_DIR` between worktrees served stale binaries** (wrong dev server, wrong test binary) →
  one private target dir per branch, reused across updates, deleted after merge.
- **Disk filled up twice** from build dirs → check free space before big builds; `make prune`; stop under
  ~12 GB free.
- **git rerere replayed stale resolutions** across agents → rerere is disabled for this repo.
- **An agent ran a merge in the main checkout** → `git -C <worktree>` for every git command.
- **Worktrees inherited the main checkout's `DATABASE_URL`/`SESSION_KEY`** → unset them; own compose project
  and port.
- **Squash-merged stacks conflict on their own base** → take `main`'s version of the merged PR's files.
- **Updates were slow** (cold full test runs, waiting for the maintainer to report conflicts) → pure merges run
  `make compile` only and CI gates; update open PRs right after each merge.
- **A Dioxus server function keeps running after an HTTP timeout** (it runs detached) → never answer before
  the work is done; bound work at its source (body read, pool acquire, Postgres timeouts).
- **Copilot's comments were not read** at first and contained real defects → triage them in every check.
- **Regression tests that pass without the fix** were caught only by reverting → always prove the test fails
  first.
- **Timing-based tests flake** under load → synchronise on events, assert outcomes, not wall-clock bounds.
- **Hand-drawn SVG art was rejected** → art through image-model prompts.
