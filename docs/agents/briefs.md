# Brief templates

Copy, fill the `<…>` parts, and keep the briefs short: the rules live in [implementer.md](implementer.md) and
[reviewer.md](reviewer.md). Ask for a report length (lines) every time.

## Implementer

```text
You implement ticket #<N> (<title>) of fe2o3-labs/Iron-Oxide. First read and follow CLAUDE.md,
docs/agents/implementer.md (and docs/agents/ui.md for UI work), then `gh issue view <N> -R fe2o3-labs/Iron-Oxide --comments` and
<the docs and code the ticket relies on>.
Branch `<feat|fix|chore|docs>/<N>-<topic>` from <origin/main | the base branch>, PR base <main | base branch>.
Deliver: <what, as a checklist: behaviour, edge cases, tests, docs>.
Decisions already taken: <…>. Out of scope: <…>; work in parallel on <files/areas> by other PRs: <…>.
make fmt / lint / test (+ test-db), CI green, <browser check + screenshots for UI>.
Report in ≤<8> lines: PR number, what's in / not, CI status, decisions beyond the ticket, things for the
maintainer.
```

## Independent review

```text
You are an independent reviewer of PR #<P> (ticket #<N>: <title>) on fe2o3-labs/Iron-Oxide. Read and follow
docs/agents/reviewer.md. Review the diff against <main | its base branch>. Correctness only; prove findings.
Answer:
1. <a concrete question: invariant, race, edge case, isolation, error mapping, idempotency, …>
2. <…>
Also triage Copilot's inline comments on the PR.
Run make lint and make test (+ test-db / a browser run where relevant). Post ONE review with the verdict line.
Report back in ≤<6> lines.
```

## Fix round (to the same implementer, by message)

```text
The review on PR #<P> (`gh pr view <P> -R fe2o3-labs/Iron-Oxide --comments`, the review starting "Verdict: correctness defect(s) found")
found: <list>. Decisions: <how each must be fixed, or "reject X because …">.
Fix in new commits (no force-push), each with a regression test that fails without the fix.
make fmt / lint / test, push, CI green. Reply in ≤<4> lines.
```

## Narrow re-check

```text
Narrow re-check, correctness only, of commit(s) <sha> on PR #<P> (ticket #<N>). Follow docs/agents/reviewer.md.
The previous review is on the PR. Answer only: (1) is each finding fixed without a new defect, (2) <specific
risk of the fix>, (3) does each regression test fail without its fix? Post ONE review with the verdict line.
Report back in ≤4 lines.
```

## Update from main (to the same implementer, by message)

```text
#<M> was squash-merged into main; PR #<P> now <conflicts | is behind>. Merge origin/main into your branch
(for files of merged PRs keep main's version unless you changed them on purpose), check
`git diff origin/main...HEAD` is only #<N>'s changes, `make compile`, push, CI is the gate.
<For a stack: then merge your branch into the next one up.> Reply in ≤2 lines.
```
