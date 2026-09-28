#!/usr/bin/env bash
# Fixtures for every guarded active surface and every historical allowance.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-retired-cli-usage.sh"
FIXTURES="$REPO_ROOT/tests/retired-cli-usage/fixtures"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-retired-cli-tests-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0
ok() { echo "PASS: $1"; passes=$((passes + 1)); }
bad() { echo "FAIL: $1" >&2; failures=$((failures + 1)); }

new_tree() {
  local tree="$TMP_ROOT/$1"
  mkdir -p "$tree/scripts"
  cp "$CHECKER" "$tree/scripts/"
  echo "$tree"
}

expect_success() {
  local name="$1" tree="$2" output
  if output="$(cd "$tree" && bash scripts/check-retired-cli-usage.sh 2>&1)"; then
    ok "$name"
  else
    bad "$name was rejected: $output"
  fi
}

expect_failure() {
  local name="$1" expected="$2" tree="$3" output
  if output="$(cd "$tree" && bash scripts/check-retired-cli-usage.sh 2>&1)"; then
    bad "$name was accepted"
  elif grep -Fq -- "$expected" <<<"$output"; then
    ok "$name"
  else
    bad "$name failed without '$expected': $output"
  fi
}

baseline_tree() {
  bash "$1/scripts/check-retired-cli-usage.sh" --update-baseline >/dev/null
}

tree="$(new_tree clean)"
baseline_tree "$tree"
expect_success "clean tree passes" "$tree"

tree="$(new_tree allowed)"
mkdir -p "$tree/src"
cp "$FIXTURES/allowed/scoped_names.rs" "$tree/src/"
cp "$FIXTURES/allowed/current_commands.sh" "$tree/scripts/"
baseline_tree "$tree"
expect_success "scoped names and current commands pass" "$tree"

prohibited_case() {
  local name="$1" fixture="$2" destination="$3" expected="$4" tree
  tree="$(new_tree "case-${name// /-}")"
  baseline_tree "$tree"
  mkdir -p "$tree/$(dirname "$destination")"
  cp "$FIXTURES/prohibited/$fixture" "$tree/$destination"
  expect_failure "$name" "$expected" "$tree"
}

prohibited_case "br in source" source_br_comment.rs src/claim.rs "src/claim.rs"
prohibited_case "bf in source" source_bf_string.rs src/prompt.rs "src/prompt.rs"
prohibited_case "bf in script" script_bf.sh scripts/queue.sh "scripts/queue.sh"
prohibited_case "br in test" test_br.rs tests/frontier.rs "tests/frontier.rs"
prohibited_case "bf in prompt" prompt_bf.yaml src/prompt/template.yaml "src/prompt/template.yaml"
prohibited_case "br in active documentation" doc_active.md docs/instructions.md "docs/instructions.md"

tree="$(new_tree historical)"
mkdir -p "$tree/docs"
cp "$FIXTURES/historical/doc_marked.md" "$tree/docs/incident.md"
baseline_tree "$tree"
expect_success "unified historical marker allows both retired commands" "$tree"

tree="$(new_tree legacy-marker)"
mkdir -p "$tree/docs"
cp "$FIXTURES/historical/doc_legacy_br_marker.md" "$tree/docs/incident.md"
baseline_tree "$tree"
expect_success "legacy historical marker remains supported" "$tree"

tree="$(new_tree missing-notice)"
mkdir -p "$tree/docs"
cp "$FIXTURES/historical/doc_marked.md" "$tree/docs/incident.md"
sed -i '/Historical\/non-operational reference/d' "$tree/docs/incident.md"
baseline_tree "$tree"
expect_failure "marker without visible notice is rejected" "docs/incident.md" "$tree"

tree="$(new_tree baseline)"
mkdir -p "$tree/src"
printf '%s\n' '// Legacy narration: the old worker ran br update on requeue.' \
  >"$tree/src/legacy.rs"
baseline_tree "$tree"
printf '%s\n' '// New instruction: bf release the bead after retry.' \
  >>"$tree/src/legacy.rs"
expect_failure "new reference beyond baseline is rejected" "src/legacy.rs" "$tree"

echo "retired CLI usage fixtures: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]

