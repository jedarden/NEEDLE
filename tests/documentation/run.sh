#!/usr/bin/env bash
# Regression tests for the retired-br documentation guard.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-documentation.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-documentation-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0

ok() {
  echo "PASS: $1"
  passes=$((passes + 1))
}

bad() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

expect_success() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    ok "$name"
  else
    bad "$name"
  fi
}

expect_failure() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    bad "$name was accepted"
  else
    ok "$name"
  fi
}

fixture_root="$TMP_ROOT/fixture"
mkdir -p "$fixture_root/docs"

cat >"$fixture_root/docs/active.md" <<'EOF'
# Active instructions

Run `bead list --ready` to inspect available work.
EOF
expect_success "current bead commands are accepted" \
  env NEEDLE_DOCUMENTATION_ROOT="$fixture_root" "$CHECKER"

cat >"$fixture_root/docs/active.md" <<'EOF'
# Active instructions

The retired `br` CLI must not appear in active instructions.
EOF
expect_failure "unmarked retired commands are rejected" \
  env NEEDLE_DOCUMENTATION_ROOT="$fixture_root" "$CHECKER"

cat >"$fixture_root/docs/historical.md" <<'EOF'
# Incident excerpt

> Historical/non-operational reference: this document preserves retired `br`
> commands from an earlier implementation or incident. Do not run them; use
> the current `bead` CLI for active work.
<!-- retired-br: historical-only -->

The incident used `br ready` before the migration.
EOF
rm -f "$fixture_root/docs/active.md"
expect_success "marked historical excerpts are accepted" \
  env NEEDLE_DOCUMENTATION_ROOT="$fixture_root" "$CHECKER"

echo "documentation checker tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
