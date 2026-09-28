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
- CI runs [gitleaks](https://github.com/gitleaks/gitleaks) over the full git history on every push
  and pull request. A detected secret fails the build.
- Scan before you push: `gitleaks git --redact` (committed history) and
  `gitleaks dir --redact .` (working tree, including untracked files).
- If a secret is ever committed, treat it as leaked: **revoke and rotate it first**, then remove
  it from the code. Rewriting git history does not make a pushed secret safe again.
