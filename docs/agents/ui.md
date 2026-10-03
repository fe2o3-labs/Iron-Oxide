# UI work

The maintainer chose the design direction **A · Forge** (dark + light, follows `prefers-color-scheme`,
dark-first) on 2026-09-28; the reference boards and the token table are on #26. The implementation is the
single stylesheet `crates/iron-oxide-app/assets/app.css` (CSS custom properties per theme) and the components in
`crates/iron-oxide-app/src/ui/components/`; the gallery at `/dev/components` (debug builds only) shows every
component in both themes. Use them; don't fork or restyle them. Extend a component minimally when it lacks
something, and say so in the PR.

## Look and feel

- Fonts, self-hosted (no runtime third-party requests; the PWA works offline): **Big Shoulders Display**
  700/900 for numbers, titles and big buttons (titles uppercase), **IBM Plex Sans** for body text,
  **JetBrains Mono** 500/700 for labels (uppercase, letter-spacing ~0.12em).
- Accent `#E8703A`; accent text `#F08A4B` (dark) / `#B04E1A` (light). Ground `#121416` / `#F1EDE6`, surface
  `#1B1E22` / `#FFFFFF`. The full table is in `app.css` and [docs/palette.md](../palette.md).
- Sizes: steppers 72 × 72 px, the Done button 76 px high, cards radius 20, **tap targets ≥ 56 px** (44 px only for
  header icon buttons), big numbers 96 px, rest countdown 168 px. WCAG AA contrast in both themes, plus a
  non-colour cue (≥ 3:1) for selected states.
- Mobile-first, one-handed, gym use: big targets, minimal typing, 390 × 844 as the reference viewport, safe-area
  insets, no horizontal scroll, the bottom nav never covers content.

## Behaviour

- **Errors are always shown**, never swallowed: every server-function error goes through
  `ApiFailure::classify` ([docs/api.md](../api.md)) to the banner — 401 → sign-in, 403 plan limit with its
  message, 404, 409, 422 per field path, 429 with the server's wait text, 503 retryable, 408 retried.
- **Offline-first:** the app opens without a connection with a visible notice and keeps retrying; session writes
  go through the offline outbox (`src/offline.rs`, `use_outbox()`), with client UUIDv7 ids reused on retries.
- Plan gates come from the user's entitlements (`my_entitlements`), never from a guess on a 403.
- Weights: the domain `Weight` and the user's unit (kg/lb) via the shared weight formatting; never floats in
  logic. e1RM shown rounded to 0.5 kg / 1 lb.
- Domain logic (rotation, progression, plates, stats, timers) comes from `iron-oxide-domain`; the UI holds thin
  view models with unit tests.

## Checking UI work

`make dev` (<http://localhost:8080>), a real browser at 390 × 844 in both themes, sign-in with the browser's
virtual authenticator; screenshots listed in the PR. Pictorial art (icons, illustrations) is never hand-drawn:
see rule 7 in [CLAUDE.md](../../CLAUDE.md).
