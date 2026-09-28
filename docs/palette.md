# Colour palette

Iron Oxide is dark-first. It is meant to be read on a phone, at arm's length, under gym lighting. The palette is iron grey surfaces, rust-orange primary, oxide-red accents and warm off-white text.

The tokens live in `crates/iron-oxide-app/assets/tokens.css` as CSS custom properties (`--io-*`), loaded on every page by the root component. Use the tokens and never hard-code hex values in components. The same `#141619` is also hard-coded in `crates/iron-oxide-app/public/manifest.webmanifest` (`theme_color`, `background_color`) and in `THEME_COLOR` in `crates/iron-oxide-app/src/pwa.rs` (the `theme-color` meta tag), so change them together.

## Tokens

Contrast ratios are WCAG 2.x, measured against `bg` / `surface` / `surface-2`. AA requires 4.5:1 for normal text, and 3:1 for large text and for UI component boundaries.

| Token | Hex | Use | Contrast |
|---|---|---|---|
| `--io-bg` | `#141619` | App background, manifest theme/background colour | |
| `--io-surface` | `#1d2024` | Cards, sheets | |
| `--io-surface-2` | `#272b30` | Raised elements, inputs, pressed states | |
| `--io-border` | `#3b4047` | Decorative dividers only | 1.7 (not for meaning) |
| `--io-border-strong` | `#707780` | Input and control outlines | 4.0 / 3.6 / 3.2 |
| `--io-text` | `#f4efe8` | Body text | 15.9 / 14.3 / 12.5 |
| `--io-text-muted` | `#b9b1a7` | Secondary text, labels | 8.6 / 7.7 / 6.7 |
| `--io-primary` | `#e8703a` | Rust orange: primary buttons, active states, primary icons | 5.9 / 5.3 / 4.6 |
| `--io-primary-strong` | `#f08a4b` | Hover/pressed primary, focus ring | 7.3 / 6.6 / 5.7 |
| `--io-on-primary` | `#141619` | Text on primary fills | 5.9 on primary, 7.3 on primary-strong |
| `--io-accent` | `#b3362b` | Oxide red **fills only**: badges, PR highlights, destructive buttons | 3.0 / 2.7 / 2.4 (not for text) |
| `--io-accent-strong` | `#8f2a21` | Pressed accent fill | |
| `--io-on-accent` | `#f4efe8` | Text on accent fills | 5.3 on accent, 7.3 on accent-strong |
| `--io-accent-text` | `#f06a5b` | Oxide red as text or icon; also `--io-danger` | 6.0 / 5.4 / 4.7 |
| `--io-success` | `#5fbf7f` | Success text/icons | 8.0 / 7.2 / 6.3 |
| `--io-warning` | `#e8b04a` | Warning text/icons | 9.3 / 8.4 / 7.3 |
| `--io-focus` | = `primary-strong` | Focus outline | |

## Rules

- Text on a primary button uses `--io-on-primary` (dark), not white: white on rust orange is only 2.95:1.
- Never put `--io-accent` text on a dark surface. Use `--io-accent-text`.
- Every pair in the table passes AA for normal text, except the rows marked otherwise.

## Icon

The app icon is a front-on barbell grip plate: a rust-orange plate shading into oxide red, three grip slots, and an iron hub on an iron-grey background. The SVG sources live in `crates/iron-oxide-app/icons/`. `icons/render.sh` regenerates the PNGs and `favicon.ico` in `public/` (it needs `rsvg-convert` and ImageMagick). The maskable variant keeps the plate inside the central 80% safe zone, and the apple-touch icon is opaque because iOS applies its own mask.
