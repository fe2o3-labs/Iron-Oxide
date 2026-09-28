#!/usr/bin/env bash
# Guard against Postgres tests silently not running (a renamed module, a filter, a feature flag).
# Reads the output of `cargo test -p iron-oxide-app --features server -- --ignored`, saved to a
# file, and checks that:
#   1) every `#[ignore = "needs Postgres"]` test in the app's src/ passed;
#   2) every isolation and schema test is among them: the schema tests by module, the repository
#      isolation tests by naming convention.
# Called by `make test-db` (and so by CI's integration job). Run it from the repository root.
set -euo pipefail

log=${1:?usage: scripts/check-postgres-tests.sh <cargo test output>}

expected=$(grep -rE '^\s*#\[ignore = "needs Postgres"\]' crates/iron-oxide-app/src | wc -l | tr -d ' ')
passed=$(grep -cE '^test server::\S+ \.\.\. ok$' "$log" || true)
echo "Postgres tests in src/: $expected, passed: $passed"
if [ "$expected" -lt 1 ] || [ "$passed" -ne "$expected" ]; then
  echo "::error::$passed of $expected Postgres tests passed"
  exit 1
fi
schema=$(grep -cE '^test server::db::schema_tests::\S+ \.\.\. ok$' "$log" || true)
isolation=$(grep -cE '^test server::db::\w+::tests::\w*(another_users|users_only|nobody_can|two_users|only_the_users)\w* \.\.\. ok$' "$log" || true)
echo "schema tests passed: $schema, isolation tests passed: $isolation"
# Floors: today's counts. Raise them when adding tests; lowering one needs a reason.
if [ "$schema" -lt 20 ] || [ "$isolation" -lt 10 ]; then
  echo "::error::isolation or schema tests missing (schema $schema < 20 or isolation $isolation < 10)"
  exit 1
fi
