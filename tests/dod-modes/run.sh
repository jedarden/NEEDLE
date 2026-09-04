#!/usr/bin/env bash
# Tests for the Definition of Done's CI modes: --gate and --target.
#
# --gate decides whether a failed fast lane still compiles test targets.
# Getting it wrong in the strict direction is merely slow; getting it wrong in
# the permissive direction silently reverts needle-ci to compiling a full test
# suite for code that has already been rejected -- the 14.8 pod-hours of
# 2026-09-03. So the predicate is tested, not trusted.
#
# The functions are extracted from the real script rather than copied, so this
# cannot drift away from what actually runs.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DOD="$REPO_ROOT/scripts/definition-of-done.sh"
[[ -f "$DOD" ]] || { echo "missing $DOD" >&2; exit 1; }

extracted="$(mktemp "${TMPDIR:-/tmp}/dod-modes-XXXXXX.sh")"
for fn in needle_slow_targets needle_cargo_selector selected_cargo_targets needle_gate_skips_slow_lane; do
  awk -v f="^${fn}\\\\(\\\\)" '$0 ~ f, /^}/' "$DOD" >> "$extracted"
done
# Every function must have been found, or the test would silently pass.
for fn in needle_slow_targets needle_cargo_selector selected_cargo_targets needle_gate_skips_slow_lane; do
  grep -q "^${fn}()" "$extracted" || { echo "FAIL: could not extract $fn from $DOD" >&2; exit 1; }
done
# shellcheck source=/dev/null
source "$extracted"

pass=0
fail=0

ok()   { pass=$((pass + 1)); echo "  ok   $1"; }
bad()  { fail=$((fail + 1)); echo "  FAIL $1"; }

# assert_lines <desc> <want-count> <command...>
assert_lines() {
  local desc="$1" want="$2"
  shift 2
  local got
  got="$("$@" 2>/dev/null | grep -c . || true)"
  if [[ "$got" == "$want" ]]; then ok "$desc"; else bad "$desc (expected $want lines, got $got)"; fi
}

# assert_selector <desc> <expected-args> <name>
assert_selector() {
  local desc="$1" want="$2" name="$3" got
  got="$(needle_cargo_selector "$name" 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"
  if [[ "$got" == "$want" ]]; then ok "$desc"; else bad "$desc (expected '$want', got '$got')"; fi
}

# assert_fails <desc> <command...>
assert_fails() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then bad "$desc (expected non-zero, got 0)"; else ok "$desc"; fi
}

echo "=== Definition of Done modes ==="

# ── needle_slow_targets ──────────────────────────────────────────────────────
assert_lines "target table lists six names" 6 needle_slow_targets

# Exact set, asserted literally: a renamed or dropped target must be caught
# here rather than silently accepted by every consumer of the table.
WANT_TABLE="$(printf '%s\n' lib integration_tests p2_integration_tests p3_integration_tests real_br_integration_tests installer)"
GOT_TABLE="$(needle_slow_targets)"
if [[ "$GOT_TABLE" == "$WANT_TABLE" ]]; then
  ok "target table is exactly the five cargo targets plus installer"
else
  bad "target table drifted (got: $(echo "$GOT_TABLE" | tr '\n' ' '))"
fi

# ── needle_cargo_selector ────────────────────────────────────────────────────
assert_selector "lib selects the unit-test target" "--lib" lib
assert_selector "integration_tests selects its target" "--test integration_tests" integration_tests
assert_selector "p2 selects its target" "--test p2_integration_tests" p2_integration_tests
assert_selector "p3 selects its target" "--test p3_integration_tests" p3_integration_tests
assert_selector "real_br selects its target" "--test real_br_integration_tests" real_br_integration_tests
assert_fails "unknown target has no selector" needle_cargo_selector nope
assert_fails "installer is not a cargo target" needle_cargo_selector installer

# ── selected_cargo_targets ───────────────────────────────────────────────────
SLOW_TARGET=""
assert_lines "default selection is all five cargo targets" 5 selected_cargo_targets

SLOW_TARGET="integration_tests"
GOT_TARGET="$(selected_cargo_targets)"
if [[ "$GOT_TARGET" == "integration_tests" ]]; then
  ok "a single --target selects only that target"
else
  bad "a single --target should select only that target (got: $GOT_TARGET)"
fi

SLOW_TARGET="installer"
assert_lines "--target installer selects no cargo target" 0 selected_cargo_targets

SLOW_TARGET=""

# ── needle_gate_skips_slow_lane ──────────────────────────────────────────────
FAILURES=("cargo clippy: exit code 101")
GATE=true
if needle_gate_skips_slow_lane; then ok "gate suppresses the slow lane after a fast-lane failure"
else bad "gate should suppress the slow lane after a fast-lane failure"; fi

GATE=false
if needle_gate_skips_slow_lane; then bad "aggregate mode must not suppress the slow lane"
else ok "aggregate mode does not suppress the slow lane"; fi

FAILURES=()
GATE=true
if needle_gate_skips_slow_lane; then bad "gate must not suppress the slow lane when the fast lane passed"
else ok "gate does not suppress the slow lane when the fast lane passed"; fi

# A --changed-only failure attributed to somebody else's file lands in
# PREEXISTING, not FAILURES, so it must not gate the slow lane either.
GATE=true
FAILURES=()
PREEXISTING=("cargo clippy")
if needle_gate_skips_slow_lane; then bad "a pre-existing failure must not gate the slow lane"
else ok "a pre-existing failure does not gate the slow lane"; fi

echo ""
echo "passed: $pass  failed: $fail"
[[ "$fail" -eq 0 ]]
