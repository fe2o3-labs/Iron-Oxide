# Security policy

## Reporting a vulnerability

Please **do not** open a public issue, discussion or pull request for a security problem.

Report it privately through GitHub's security advisories:
[Report a vulnerability](https://github.com/guizmaii-opensource/Iron-Oxide/security/advisories/new)
(repository **Security** tab → **Report a vulnerability**).

Include what you can: the affected component, steps to reproduce, the impact you expect, and a
suggested fix if you have one. You will get an acknowledgement as soon as the maintainer sees the
report, and the advisory is published once a fix is released.

Only the latest version on `main` is supported.

## Secrets in this repository

The repository is public and built in the open.

- Never commit secrets, credentials, private keys or real `.env` files. Configuration comes from
  environment variables; `.env.example` holds placeholders only.
- CI runs [gitleaks](https://github.com/gitleaks/gitleaks) on every pull request (the PR's commits,
  including what merge commits change) and every push to `main` (the pushed commits), and scans the
  full history of `main` weekly. A detected secret fails the build. GitHub secret scanning and push
  protection are also enabled on the repository.
- Scan before you push: `gitleaks git --redact` (committed history) and
  `gitleaks dir --redact .` (working tree, including untracked files).
- If a secret is ever committed, treat it as leaked: **revoke and rotate it first**, then remove
  it from the code. Rewriting git history does not make a pushed secret safe again.

### Test fixtures that look like secrets

Test data sometimes looks like a credential to gitleaks. First make sure it really is fake. Then,
in order of preference:

1. Use an obviously fake value that doesn't match a secret pattern (for example `"test-secret"`).
2. Add an inline `gitleaks:allow` comment on that line:
   `let secret = "…"; // gitleaks:allow (test fixture, not a real key)`.
3. If the line can't be edited (for example, the finding is in an already-pushed commit), add its
   fingerprint to `.gitleaksignore`, with a comment line above it that says why it is not a
   secret. The fingerprint is printed by gitleaks: `<commit>:<file>:<rule-id>:<line>`. It contains
   the commit SHA, and PRs are squash-merged, which creates a new commit on `main`: a fingerprint
   of a PR commit stops matching once the PR is merged. So use it only for findings in commits
   that are already on `main`, and prefer options 1 and 2.

Never use these to silence a real secret. A real secret must be revoked and rotated.
