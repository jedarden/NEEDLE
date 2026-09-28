#!/usr/bin/env bash
# Mutation tests for the active retired-CLI addition policy.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-retired-cli-additions.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-retired-cli-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0
cli_name="$(printf '\142\162')"
legacy_cli_name="$(printf '\142\146')"
historical_notice='> Historical/non-operational reference: this file preserves retired CLI examples.'
historical_marker="<!-- retired-${cli_name}: historical-only -->"

ok() {
  echo "PASS: $1"
  passes=$((passes + 1))
}

bad() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

run_checker() {
  local root="$1"
  env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
    NEEDLE_RETIRED_CLI_ROOT="$root" "$CHECKER"
}

expect_rejected() {
  local name="$1" root="$2" output="$TMP_ROOT/output"
  if run_checker "$root" >"$output" 2>&1; then
    bad "$name was accepted"
  elif grep -Fq "new retired CLI reference" "$output"; then
    ok "$name"
  else
    bad "$name failed for an unrelated reason: $(cat "$output")"
  fi
}

fixture="$TMP_ROOT/workspace"
fixture_git() {
  env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE git -C "$fixture" "$@"
}

mkdir -p "$fixture/src" "$fixture/prompts" "$fixture/scripts" \
  "$fixture/tests" "$fixture/docs"
printf 'fn active() {}\n' >"$fixture/src/active.rs"
printf '# Current prompt\n' >"$fixture/prompts/dispatch.md"
printf '#!/bin/sh\n' >"$fixture/scripts/active.sh"
printf 'fn smoke() {}\n' >"$fixture/tests/active.rs"
printf '# Current guidance\n' >"$fixture/docs/current.md"
fixture_git init -q
fixture_git config user.name "NEEDLE policy test"
fixture_git config user.email "needle-policy-test@example.invalid"
fixture_git add -f src/active.rs prompts/dispatch.md \
  scripts/active.sh tests/active.rs docs/current.md
fixture_git commit -qm "policy fixture baseline"

printf '\n// %s ready\n' "$cli_name" >>"$fixture/src/active.rs"
printf '\nUse %s ready to inspect work.\n' "$cli_name" >>"$fixture/prompts/dispatch.md"
printf '\n%s close ITEM\n' "$cli_name" >>"$fixture/scripts/active.sh"
printf '\n// %s list\n' "$cli_name" >>"$fixture/tests/active.rs"
printf '\nRun `%s ready` before dispatch.\n' "$cli_name" >>"$fixture/docs/current.md"
expect_rejected "new active references across source, prompts, scripts, tests, and docs" \
  "$fixture"

fixture_git checkout -- src prompts scripts tests docs
printf '\n// %s list\n' "$legacy_cli_name" >>"$fixture/src/active.rs"
printf '\nUse %s close ITEM.\n' "$legacy_cli_name" >>"$fixture/prompts/dispatch.md"
printf '\n%s ready --json\n' "$legacy_cli_name" >>"$fixture/scripts/active.sh"
printf '\n// %s dep add BLOCKER\n' "$legacy_cli_name" >>"$fixture/tests/active.rs"
printf '\nRun `%s show ITEM` before dispatch.\n' "$legacy_cli_name" >>"$fixture/docs/current.md"
expect_rejected "new active legacy-CLI references across source, prompts, scripts, tests, and docs" \
  "$fixture"

fixture_git checkout -- src prompts scripts tests docs
printf '%s\n%s\n\nIncident command: `%s ready`.\n' \
  "$historical_notice" "$historical_marker" "$cli_name" \
  >"$fixture/docs/history.md"
fixture_git add docs/history.md
fixture_git commit -qm "add marked historical fixture"
printf '\nThe old run also called `%s close ITEM`.\n' "$cli_name" \
  >>"$fixture/docs/history.md"
printf 'The migrated history also records `%s sync --flush-only`.\n' \
  "$legacy_cli_name" >>"$fixture/docs/history.md"
if run_checker "$fixture" >/dev/null 2>&1; then
  ok "marked historical documentation is allowlisted"
else
  bad "marked historical documentation was rejected: $(run_checker "$fixture" 2>&1)"
fi

printf '%s\n%s\nRun `%s ready`.\n' \
  "$historical_notice" "$historical_marker" "$cli_name" \
  >>"$fixture/prompts/dispatch.md"
expect_rejected "historical markers do not allowlist active prompts" "$fixture"

printf '# Current guidance\n\nRun `%s ready`.\n' "$cli_name" \
  >"$fixture/docs/current.md"
expect_rejected "unmarked documentation references are rejected" "$fixture"

echo "retired CLI addition checker: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
