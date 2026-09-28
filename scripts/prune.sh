#!/usr/bin/env bash
# Deletes old build artefacts from the cargo target dir this checkout uses. Run by `make prune`.
#
#   PRUNE_DAYS=14        artefacts and incremental caches unused for longer are deleted
#   PRUNE_MAXSIZE=10GB   optional: then the oldest artefacts go until the dir fits
#   DRY_RUN=1            only show what would be deleted
#
# Steps, in this order:
#   1. incremental caches (`*/incremental/*`) older than PRUNE_DAYS: cargo sweep never removes
#      them, and they are always safe to delete (the next build is just less incremental);
#   2. `cargo sweep --time PRUNE_DAYS`, then `cargo sweep --installed` (other toolchains);
#   3. with PRUNE_MAXSIZE: every remaining incremental cache first, then `cargo sweep --maxsize`.
#      The cap is measured on the whole dir, including dx/ and doc/, which cargo sweep never
#      removes; if those alone exceed it, the dir cannot fit.
#
# The target dir is the one cargo resolves (CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR,
# build.target-dir, else ./target), canonicalised. It must look like a cargo target dir and must
# not be /, $HOME, the checkout or one of its ancestors. cargo sweep only deletes build artefacts,
# never sources, `.sqlx/` or anything tracked.
set -euo pipefail

CARGO=${CARGO:-cargo}
PRUNE_DAYS=${PRUNE_DAYS:-14}
PRUNE_MAXSIZE=${PRUNE_MAXSIZE:-}
DRY_RUN=${DRY_RUN:-0}

fail() {
  echo "make prune: $*" >&2
  exit 1
}

# --- Arguments, validated before anything is deleted ----------------------------------------
[[ $PRUNE_DAYS =~ ^[0-9]+$ ]] || fail "PRUNE_DAYS must be a whole number of days, not '$PRUNE_DAYS'."
if [ -n "$PRUNE_MAXSIZE" ] &&
  ! [[ $PRUNE_MAXSIZE =~ ^([0-9]+|[0-9]+(\.[0-9]+)?(B|kB|KB|MB|GB|TB|KiB|MiB|GiB|TiB))$ ]]; then
  fail "PRUNE_MAXSIZE='$PRUNE_MAXSIZE' is not a size: use a number of MB (500) or a number with a unit, no space (10GB, 1.5GB, 800MiB; B, kB, MB, GB, TB, KiB, MiB, GiB, TiB)."
fi

# --- The target dir cargo (and so cargo sweep) actually uses ---------------------------------
repo=$(pwd -P)
metadata=$("$CARGO" metadata --format-version 1 --no-deps 2>/dev/null) ||
  fail "cannot read the target dir from 'cargo metadata'."
raw=$(printf '%s' "$metadata" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[ -n "$raw" ] || fail "cargo metadata did not report a target dir."
if [ ! -e "$raw" ]; then
  echo "Nothing to prune: $raw does not exist."
  exit 0
fi
[ -d "$raw" ] || fail "$raw is not a directory."
target=$(cd "$raw" && pwd -P)
home=$(cd "$HOME" 2>/dev/null && pwd -P || printf '%s' "$HOME")

[ "$target" != / ] || fail "refusing to sweep /."
[ "$target" != "$home" ] || fail "refusing to sweep \$HOME ($target)."
case "$repo/" in
  "$target"/*) fail "refusing to sweep $target: it is this checkout or one of its parents." ;;
esac
if ! [ -e "$target/.rustc_info.json" ] && ! [ -e "$target/CACHEDIR.TAG" ] &&
  ! [ -d "$target/debug" ] && ! [ -d "$target/release" ]; then
  fail "refusing to sweep $target: it does not look like a cargo target dir (no .rustc_info.json, CACHEDIR.TAG, debug/ or release/)."
fi

"$CARGO" sweep --version >/dev/null 2>&1 || fail "cargo-sweep is not installed. Run 'make setup'."

# Size in KB; 0 when du cannot tell (unreadable entries, files removed while it runs).
size_kb() {
  local kb
  kb=$(du -sk "$1" 2>/dev/null | awk 'NR == 1 { print $1 }') || true
  case "$kb" in '' | *[!0-9]*) kb=0 ;; esac
  printf '%s' "$kb"
}
# Deletes (or lists, in a dry run) the entries of every incremental cache under the target dir;
# $1 is a find(1) age filter, e.g. "-mtime +14", or empty for all of them.
prune_incremental() {
  local age=$1 dir entry
  while IFS= read -r dir; do
    # shellcheck disable=SC2086 # $age is deliberately split into find arguments
    while IFS= read -r entry; do
      if [ "$DRY_RUN" = 1 ]; then
        echo "would delete $entry"
      else
        rm -rf -- "$entry"
      fi
    done < <(find "$dir" -mindepth 1 -maxdepth 1 $age -print)
  done < <(find "$target" -maxdepth 3 -type d -name incremental -print)
}

dry=()
if [ "$DRY_RUN" = 1 ]; then dry=(--dry-run); fi
before=$(size_kb "$target")
echo "==> $target: incremental caches older than $PRUNE_DAYS days"
prune_incremental "-mtime +$PRUNE_DAYS"
echo "==> cargo sweep: artefacts older than $PRUNE_DAYS days, then those of removed toolchains"
"$CARGO" sweep ${dry[@]+"${dry[@]}"} --time "$PRUNE_DAYS" .
"$CARGO" sweep ${dry[@]+"${dry[@]}"} --installed .
if [ -n "$PRUNE_MAXSIZE" ]; then
  echo "==> size cap $PRUNE_MAXSIZE: every incremental cache first, then the oldest artefacts"
  prune_incremental ""
  "$CARGO" sweep ${dry[@]+"${dry[@]}"} --maxsize "$PRUNE_MAXSIZE" .
fi
after=$(size_kb "$target")
echo "$target: $((before / 1024)) MB -> $((after / 1024)) MB$([ "$DRY_RUN" = 1 ] && echo ' (dry run)')"
