#!/usr/bin/env bash
# Mutation tests for the whole-tree retired-CLI guard.
#
# Like the guard itself, this file never spells the retired CLI's name or its
# legacy handle as literals: every fixture string is assembled at runtime, and
# the repo copy of this suite must stay invisible to both retired-CLI checks.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Assembled for the same reason as everywhere else: the repo copy of this
# file must not contain the retired token the addition policy scans for.
CHECKER="$REPO_ROOT/scripts/check-retired-$(printf '\142\162').sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-retired-cli-tree-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0
cli_name="$(printf '\142\162')"
legacy_handle="$(printf '\102\122')_BIN"
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
    NEEDLE_RETIRED_BR_ROOT="$root" bash "$CHECKER"
}

expect_rejected() {
  local name="$1" root="$2" output="$TMP_ROOT/output"
  if run_checker "$root" >"$output" 2>&1; then
    bad "$name was accepted"
  elif grep -Fq "active retired-CLI command" "$output"; then
    ok "$name"
  else
    bad "$name failed for an unrelated reason: $(cat "$output")"
  fi
}

expect_stale_failure() {
  local name="$1" root="$2" output="$TMP_ROOT/output"
  if run_checker "$root" >"$output" 2>&1; then
    bad "$name was accepted"
  elif grep -Fq "stale grandfather entry" "$output"; then
    ok "$name"
  else
    bad "$name failed for an unrelated reason: $(cat "$output")"
  fi
}

expect_accepted() {
  local name="$1" root="$2" output="$TMP_ROOT/output"
  if run_checker "$root" >"$output" 2>&1 &&
    grep -Fq "no active retired commands" "$output"; then
    ok "$name"
  else
    bad "$name was rejected: $(cat "$output")"
  fi
}

# A clean baseline tree: active surfaces with no retired references, plus
# retired commands that live only in records and session captures, which are
# out of scope by design. No .git directory, so listing uses the find
# fallback that the archive-extraction gate also exercises.
make_fixture() {
  local root="$1"
  mkdir -p "$root/src/prompt" "$root/scripts" "$root/tests/harness" \
    "$root/docs" "$root/.beads" "$root/notes"
  printf 'Dispatch the claimed bead with the current CLI.\n' \
    >"$root/src/prompt/dispatch.md"
  printf '#!/bin/sh\nexit 0\n' >"$root/scripts/smoke.sh"
  printf '#!/bin/sh\nexit 0\n' >"$root/tests/harness/run.sh"
  printf '# Current guidance\n' >"$root/docs/current.md"
  printf '{"note":"incident used %s close %s"}\n' "$cli_name" 'x-1' \
    >"$root/.beads/forensic.jsonl"
  printf 'session capture: ran %s ready --json\n' "$cli_name" \
    >"$root/notes/session.txt"
}

# 1. Baseline: find fallback, records out of scope.
baseline="$TMP_ROOT/baseline"
make_fixture "$baseline"
expect_accepted "clean tree passes with records and captures out of scope" "$baseline"

# 2. A retired command in an active prompt is rejected.
prompt_fixture="$TMP_ROOT/prompt"
make_fixture "$prompt_fixture"
printf 'Close the bead with `%s close %s --reason done`.\n' "$cli_name" 'x-2' \
  >>"$prompt_fixture/src/prompt/dispatch.md"
expect_rejected "retired command in an active prompt" "$prompt_fixture"

# 3. Availability probes for the retired binary are rejected.
probe_fixture="$TMP_ROOT/probe"
make_fixture "$probe_fixture"
printf 'if ! which %s >/dev/null; then exit 1; fi\n' "$cli_name" \
  >>"$probe_fixture/scripts/smoke.sh"
expect_rejected "availability probe for the retired binary" "$probe_fixture"

# 4. The legacy e2e handle is rejected outside the grandfathered suite.
handle_fixture="$TMP_ROOT/handle"
make_fixture "$handle_fixture"
printf '%s="$(command -v %s)"\n' "$legacy_handle" "$cli_name" \
  >>"$handle_fixture/tests/harness/run.sh"
expect_rejected "legacy e2e handle in a new test script" "$handle_fixture"

# 5. A document carrying the ADR-001 notice and marker keeps its history.
marked_fixture="$TMP_ROOT/marked"
make_fixture "$marked_fixture"
printf '%s\n%s\n\nIncident command: `%s ready --json`.\n' \
  "$historical_notice" "$historical_marker" "$cli_name" \
  >"$marked_fixture/docs/history.md"
expect_accepted "marked historical documentation is allowlisted" "$marked_fixture"

# 6. The same command without the marker is rejected.
unmarked_fixture="$TMP_ROOT/unmarked"
make_fixture "$unmarked_fixture"
printf 'Incident command: `%s ready --json`.\n' "$cli_name" \
  >"$unmarked_fixture/docs/current.md"
expect_rejected "unmarked documentation references are rejected" "$unmarked_fixture"

# 7. Markers do not allowlist active code.
marked_script_fixture="$TMP_ROOT/marked-script"
make_fixture "$marked_script_fixture"
{
  printf '#!/bin/sh\n%s\n%s\n%s init\n' \
    "$historical_notice" "$historical_marker" "$cli_name"
} >"$marked_script_fixture/scripts/tool.sh"
expect_rejected "historical markers do not allowlist active scripts" \
  "$marked_script_fixture"

# 8. Git mode: tracked files are listed via the index, and content is read
# from the working tree, so an uncommitted retired command still fails.
git_fixture="$TMP_ROOT/git-mode"
make_fixture "$git_fixture"
git -C "$git_fixture" init -q
git -C "$git_fixture" config user.name "NEEDLE policy test"
git -C "$git_fixture" config user.email "needle-policy-test@example.invalid"
git -C "$git_fixture" add -f src scripts tests docs
git -C "$git_fixture" commit -qm "policy fixture baseline"
printf '%s list --json\n' "$cli_name" >>"$git_fixture/tests/harness/run.sh"
expect_rejected "uncommitted retired command in a git-listed file" "$git_fixture"

# 9. Burn-down contract: a grandfathered path whose file exists but no longer
# carries any retired reference is a stale entry and fails the check. The
# path below is a real table entry in the guard.
stale_fixture="$TMP_ROOT/stale"
make_fixture "$stale_fixture"
mkdir -p "$stale_fixture/src/claim"
printf 'fn migrated() {}\n' >"$stale_fixture/src/claim/mod.rs"
expect_stale_failure "fully migrated grandfather entry is stale" "$stale_fixture"

echo "retired CLI tree policy tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
