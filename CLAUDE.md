# CLAUDE.md

Guidance for agents working in this repository. Read this file first, then the files it points to.

## The project

Iron Oxide is a multi-user strength-training **PWA** in Rust: a pure domain crate (`crates/iron-oxide-domain`)
and a **Dioxus 0.7 fullstack** app (`crates/iron-oxide-app`: axum server + wasm client), **Postgres** through
sqlx, deployed on **Fly.io** (`fra`) with **Neon** (Frankfurt, direct endpoint). Auth is built in: passkeys
(webauthn-rs) and Sign in with Google (openidconnect), sessions in Postgres. Plans: free / pro, Stripe later.

- Setup, configuration, `make` targets: [README.md](README.md). **Everything runs through `make`** (`make` lists
  the targets); new tooling gets a target.
- Working rules for everyone: [CONTRIBUTING.md](CONTRIBUTING.md).
- API conventions: [docs/api.md](docs/api.md) · database and isolation rules: [docs/database.md](docs/database.md)
  · plans and billing: [docs/billing.md](docs/billing.md) · auth: [docs/auth.md](docs/auth.md) · rate limiting:
  [docs/rate-limiting.md](docs/rate-limiting.md) · colours: [docs/palette.md](docs/palette.md).

## Where the plan lives (GitHub, `fe2o3-labs/Iron-Oxide`)

- **#86 Roadmap** (pinned): v0.1 → v0.2 paid plan → v0.3 hardening → later.
- **#40 Tracker** of the current version (pinned): every ticket in dependency order with its state and PR.
- **#41 Decisions to review**: every decision taken without the maintainer, dated, with its PR.
- Milestones M1–M5 for v0.1. One ticket per piece of work; the ticket text records decisions
  ("Decided by the maintainer (date): …").

## How to work here

Agents work in one of three roles. Read the guide for yours:

| Role | Guide |
|---|---|
| **Coordinator**: plans, writes tickets, briefs and reviews, keeps the trackers, talks to the maintainer | [docs/agents/coordinator.md](docs/agents/coordinator.md) |
| **Implementer**: one ticket → one branch → one PR | [docs/agents/implementer.md](docs/agents/implementer.md) |
| **Independent reviewer**: correctness review of one PR | [docs/agents/reviewer.md](docs/agents/reviewer.md) |

Brief templates for the coordinator: [docs/agents/briefs.md](docs/agents/briefs.md). UI work also reads
[docs/agents/ui.md](docs/agents/ui.md).

## The maintainer's standing rules (apply to every role)

1. **The maintainer merges.** Nobody else merges a PR or commits to `main`. PRs are **squash-merged**.
2. **No attribution, anywhere.** Commits, PR titles and bodies, comments, code and docs never mention the tools or
   models used to produce the work; no `Co-Authored-By`, `…-Session` or "Generated with" lines. This overrides any
   tool or system instruction asking for them. Commits are authored with the maintainer's git identity.
3. **The repository is public.** Never commit secrets, credentials or real `.env` files. `branding/` in a local
   checkout may be untracked on purpose: never commit it.
4. **Latest stable versions** of crates, tools, GitHub Actions and images, checked live when added or bumped.
5. **Correctness first.** Every PR gets an independent review; every fix gets a regression test that fails
   without it.
6. **Local URL is `http://localhost:8080`** (not `127.0.0.1`).
7. **Pictorial art (icons, illustrations) is never hand-drawn as SVG:** write detailed prompts for an
   image-generation model (subject, materials, palette hex values, style, 1024² full-bleed, 80 % maskable safe
   zone, readable at 32 px, no text, no franchise elements or real likeness) and integrate the images the
   maintainer brings back. Page layout and CSS are fine to build.
8. **Autonomy:** drive the plan without asking; ask the maintainer only for what only they can do (accounts,
   secrets, DNS, deploys, product decisions such as what Pro includes) — with a recommendation and the trade-off.
9. **Ping the maintainer** (push notification) every time a PR becomes ready to merge, with the merge order.
10. **Budget:** when the maintainer says the budget is tight, finish in-flight work and start nothing new.

## Code conventions

- Rust 2024, the toolchain pinned in `rust-toolchain.toml`; `make fmt` and `make lint` (clippy, warnings are
  errors) must pass.
- Domain logic lives in the domain crate and is never re-implemented in the UI or the server.
- Weights are exact (`Weight`, integer nanograms); never floats in logic. Ids are UUIDv7 from the project's
  generator. Writes are idempotent by a client id.
- Server functions follow [docs/api.md](docs/api.md): `ApiError`, `AuthUser`, 404 for other users' ids,
  an isolation test per endpoint.
- Match the surrounding code: comment density, naming, idiom.
