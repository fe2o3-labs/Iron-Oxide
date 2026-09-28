#!/usr/bin/env bash
# Tests the Makefile's safety guards in a scratch git repository, with stub tools: nothing is
# deployed, built or deleted outside the scratch directory. Run by `make test-make` (CI: Unit tests).
#
# Covered: `make test` fails without service worker tests; `make check` fails without gitleaks
# (unless SKIP_SECRETS=1); `make deploy` refuses unless CONFIRM=1, gitleaks, a green CI run, and a
# clean `main` at exactly origin/main (not ahead, behind or diverged); `make clean` and
# `make clean-all CONFIRM=1` refuse a target dir outside the checkout without CONFIRM_SHARED=1.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

# Stub tools: they only say what they would do. `gh` answers "1 successful CI run", `gh-no-ci` "0".
stubs=$scratch/stubs
mkdir -p "$stubs"
printf '#!/bin/sh\necho "STUB fly $*"\n' >"$stubs/fly"
printf '#!/bin/sh\necho "STUB cargo $*"\n' >"$stubs/cargo"
printf '#!/bin/sh\necho "STUB gitleaks $*"\n' >"$stubs/gitleaks"
printf '#!/bin/sh\necho 1\n' >"$stubs/gh"
printf '#!/bin/sh\necho 0\n' >"$stubs/gh-no-ci"
chmod +x "$stubs"/*
unset CARGO_TARGET_DIR

# A bare origin and a checkout of main holding the Makefile.
git init --quiet --bare "$scratch/origin.git"
git init --quiet -b main "$scratch/work"
work=$scratch/work
cp "$root/Makefile" "$work/Makefile"
git -C "$work" add Makefile
git -C "$work" commit --quiet -m init
git -C "$work" remote add origin "$scratch/origin.git"
git -C "$work" push --quiet -u origin main

failures=0
# run <expect: ok|fail> <description> <pattern the output must contain> -- make arguments...
run() {
  local expect=$1 what=$2 pattern=$3
  shift 4
  local out status=0
  out=$(cd "$work" && make "$@" 2>&1) || status=$?
  local verdict=pass
  if [ "$expect" = ok ] && [ "$status" -ne 0 ]; then verdict="FAIL (exit $status)"; fi
  if [ "$expect" = fail ] && [ "$status" -eq 0 ]; then verdict="FAIL (exit 0)"; fi
  if ! grep -qF -- "$pattern" <<<"$out"; then verdict="FAIL (no \"$pattern\")"; fi
  if [ "$verdict" = pass ]; then
    echo "  ok    $what"
  else
    echo "  $verdict  $what"
    sed 's/^/        | /' <<<"$out"
    failures=$((failures + 1))
  fi
}
deploy=(deploy FLY="$stubs/fly" GH="$stubs/gh" GITLEAKS="$stubs/gitleaks" MAKE=true)

echo "make test"
run fail "fails when no service worker test exists" "no service worker tests found" -- \
  test CARGO="$stubs/cargo"
mkdir -p "$work/crates/iron-oxide-app/tests/sw"
printf "import test from 'node:test';\ntest('stub', () => {});\n" \
  >"$work/crates/iron-oxide-app/tests/sw/stub.test.mjs"
run ok "runs the service worker tests when they exist" "service worker tests" -- \
  test CARGO="$stubs/cargo"
rm -r "$work/crates"

echo "make check"
run fail "fails when gitleaks is missing" "gitleaks is not installed" -- \
  check GITLEAKS="$scratch/missing" MAKE=true
run ok "skips the scan only with SKIP_SECRETS=1" "secret scan skipped (SKIP_SECRETS=1)" -- \
  check GITLEAKS="$scratch/missing" SKIP_SECRETS=1 MAKE=true

echo "make deploy"
run fail "refuses without CONFIRM=1" "CONFIRM=1" -- "${deploy[@]}"
run fail "refuses without gitleaks" "'$scratch/missing' is not installed" -- \
  "${deploy[@]}" CONFIRM=1 GITLEAKS="$scratch/missing"
run fail "refuses SKIP_SECRETS=1" "SKIP_SECRETS=1 is not allowed" -- "${deploy[@]}" CONFIRM=1 SKIP_SECRETS=1
run fail "refuses when CI has not passed" "CI has not passed" -- \
  "${deploy[@]}" CONFIRM=1 GH="$stubs/gh-no-ci"
run ok "deploys a clean main at origin/main" "STUB fly deploy" -- "${deploy[@]}" CONFIRM=1

echo untracked >"$work/untracked.txt"
run fail "refuses uncommitted changes" "uncommitted changes" -- "${deploy[@]}" CONFIRM=1
rm "$work/untracked.txt"

git -C "$work" checkout --quiet -b other
run fail "refuses another branch" "deploy from main only" -- "${deploy[@]}" CONFIRM=1
git -C "$work" checkout --quiet main

git -C "$work" commit --quiet --allow-empty -m ahead
run fail "refuses a main ahead of origin/main" "HEAD is not origin/main" -- "${deploy[@]}" CONFIRM=1

git -C "$work" push --quiet origin main
git -C "$work" reset --quiet --hard HEAD~1
run fail "refuses a main behind origin/main" "HEAD is not origin/main" -- "${deploy[@]}" CONFIRM=1

git -C "$work" commit --quiet --allow-empty -m diverged
run fail "refuses a main diverged from origin/main" "HEAD is not origin/main" -- "${deploy[@]}" CONFIRM=1

echo "make clean / clean-all"
shared=$scratch/shared-target
run fail "clean refuses a target dir outside the checkout" "CONFIRM_SHARED=1" -- \
  clean CARGO="$stubs/cargo" CARGO_TARGET_DIR="$shared"
run fail "clean: CONFIRM=1 is not enough for a shared target dir" "CONFIRM_SHARED=1" -- \
  clean CARGO="$stubs/cargo" CARGO_TARGET_DIR="$shared" CONFIRM=1
run fail "clean-all CONFIRM=1 still refuses a shared target dir" "CONFIRM_SHARED=1" -- \
  clean-all CARGO="$stubs/cargo" CARGO_TARGET_DIR="$shared" CONFIRM=1 COMPOSE=echo
run ok "clean-all goes ahead with CONFIRM=1 CONFIRM_SHARED=1" "STUB cargo clean" -- \
  clean-all CARGO="$stubs/cargo" CARGO_TARGET_DIR="$shared" CONFIRM=1 CONFIRM_SHARED=1 COMPOSE=echo
run ok "clean accepts a relative target dir inside the checkout" "STUB cargo clean" -- \
  clean CARGO="$stubs/cargo" CARGO_TARGET_DIR=target

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures Makefile guard test(s) failed."
  exit 1
fi
echo "All Makefile guard tests passed."
