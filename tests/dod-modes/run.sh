#!/usr/bin/env bash
# Tests for the Definition of Done's aggregation and CI-mode contracts.
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
test_tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/dod-modes-tmp-root-XXXXXX")"
trap 'rm -f "$extracted"; rm -rf "$test_tmp_root"' EXIT

for fn in needle_now_ms needle_duration_event reap_orphans run_check lane_tmp cleanup_lane_tmps run_slow_cargo_check needle_slow_targets needle_cargo_selector selected_cargo_targets needle_declared_test_harnesses needle_expected_slow_targets needle_validate_slow_target_coverage needle_affected_test_targets needle_clippy_selectors needle_run_clippy needle_gate_skips_slow_lane; do
  awk -v f="^${fn}\\\\(\\\\)" '$0 ~ f, /^}/' "$DOD" >> "$extracted"
done
# Every function must have been found, or the test would silently pass.
for fn in needle_now_ms needle_duration_event reap_orphans run_check lane_tmp cleanup_lane_tmps run_slow_cargo_check needle_slow_targets needle_cargo_selector selected_cargo_targets needle_declared_test_harnesses needle_expected_slow_targets needle_validate_slow_target_coverage needle_affected_test_targets needle_clippy_selectors needle_run_clippy needle_gate_skips_slow_lane; do
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

# ── Structured phase duration ────────────────────────────────────────────────
CHECKS=()
FAILURES=()
PREEXISTING=()
CHANGED_ONLY=false
needle_failure_is_ours() { return 0; }
timing_log="$test_tmp_root/timing.log"
DOD_TIMING_PHASE=build DOD_TIMING_TARGET=lib \
  run_check "timed pass" true >"$timing_log" 2>&1
if grep -Eq '^NEEDLE_DOD_TIMING \{"phase":"build","target":"lib","duration_ms":[0-9]+,"status":"pass","exit_code":0\}$' "$timing_log"; then
  ok "run_check emits a structured duration event when timing is requested"
else
  bad "run_check did not emit its structured duration event"
fi

# ── run_check failure aggregation ────────────────────────────────────────────
# The retired configurable gate had an integration target dedicated to this
# contract. Keep the useful part of that coverage against the active DoD
# implementation itself: failures do not stop later checks, their combined
# output remains visible, and every failure is retained for the final summary.
CHECKS=()
FAILURES=()
PREEXISTING=()
aggregate_log="$test_tmp_root/aggregate.log"
aggregate_marker="$test_tmp_root/final-check-ran"

run_check "first aggregate failure" bash -c \
  'echo first-stdout; echo first-stderr >&2; exit 3' >>"$aggregate_log" 2>&1
run_check "middle aggregate failure" bash -c \
  'echo middle-stderr >&2; exit 7' >>"$aggregate_log" 2>&1
run_check "final aggregate pass" touch "$aggregate_marker" >>"$aggregate_log" 2>&1

if [[ ${#CHECKS[@]} -eq 3 && ${#FAILURES[@]} -eq 2 ]]; then
  ok "run_check retains all failures and continues through the lane"
else
  bad "run_check aggregation drifted (${#CHECKS[@]} checks, ${#FAILURES[@]} failures)"
fi
if [[ -f "$aggregate_marker" ]]; then
  ok "a passing check after failures still runs"
else
  bad "a passing check after failures was skipped"
fi
if grep -q 'first-stdout' "$aggregate_log" \
  && grep -q 'first-stderr' "$aggregate_log" \
  && grep -q 'middle-stderr' "$aggregate_log"; then
  ok "failed-check stdout and stderr remain in the report"
else
  bad "failed-check output was not retained"
fi

# ── needle_slow_targets ──────────────────────────────────────────────────────
assert_lines "target table lists seven names" 7 needle_slow_targets

# Exact set, asserted literally: a renamed or dropped target must be caught
# here rather than silently accepted by every consumer of the table.
WANT_TABLE="$(printf '%s\n' lib integration_spawn integration_tests p2_integration_tests p3_integration_tests real_br_integration_tests installer)"
GOT_TABLE="$(needle_slow_targets)"
if [[ "$GOT_TABLE" == "$WANT_TABLE" ]]; then
  ok "target table is exactly the six cargo targets plus installer"
else
  bad "target table drifted (got: $(echo "$GOT_TABLE" | tr '\n' ' '))"
fi

# ── needle_cargo_selector ────────────────────────────────────────────────────
assert_selector "lib selects the unit-test target" "--lib" lib
assert_selector "integration_spawn selects its target" "--test integration_spawn" integration_spawn
assert_selector "integration_tests selects its target" "--test integration_tests" integration_tests
assert_selector "p2 selects its target" "--test p2_integration_tests" p2_integration_tests
assert_selector "p3 selects its target" "--test p3_integration_tests" p3_integration_tests
assert_selector "real_br selects its target" "--test real_br_integration_tests" real_br_integration_tests
assert_fails "unknown target has no selector" needle_cargo_selector nope
assert_fails "installer is not a cargo target" needle_cargo_selector installer

# ── selected_cargo_targets ───────────────────────────────────────────────────
SLOW_TARGET=""
assert_lines "default selection is all six cargo targets" 6 selected_cargo_targets

WANT_DEFAULT="$(needle_expected_slow_targets | grep -vx installer)"
GOT_DEFAULT="$(selected_cargo_targets)"
if [[ "$GOT_DEFAULT" == "$WANT_DEFAULT" ]]; then
  ok "default Cargo selection covers lib and every declared [[test]] harness"
else
  bad "default Cargo selection does not match Cargo.toml (got: $(echo "$GOT_DEFAULT" | tr '\n' ' '))"
fi

if needle_validate_slow_target_coverage >/dev/null 2>&1; then
  ok "explicit slow target table matches Cargo.toml plus lib/installer"
else
  bad "explicit slow target table failed its manifest coverage invariant"
fi

if (
  needle_slow_targets() {
    printf '%s\n' lib integration_tests p2_integration_tests \
      p3_integration_tests real_br_integration_tests installer
  }
  needle_validate_slow_target_coverage
) >/dev/null 2>&1; then
  bad "coverage invariant accepted a table missing integration_spawn"
else
  ok "coverage invariant rejects an omitted declared harness"
fi

SLOW_TARGET="integration_spawn"
GOT_TARGET="$(selected_cargo_targets)"
if [[ "$GOT_TARGET" == "integration_spawn" ]]; then
  ok "a single --target selects only that target"
else
  bad "a single --target should select only that target (got: $GOT_TARGET)"
fi

SLOW_TARGET="installer"
assert_lines "--target installer selects no cargo target" 0 selected_cargo_targets

SLOW_TARGET=""

# ── fast-lane Clippy target selection ────────────────────────────────────────
WANT_HARNESSES="$(printf '%s\t%s\n' \
  integration_spawn tests/integration_spawn.rs \
  integration_tests tests/integration_tests.rs \
  p2_integration_tests tests/p2_integration_tests.rs \
  p3_integration_tests tests/p3_integration_tests.rs \
  real_br_integration_tests tests/real_br_integration_tests.rs)"
GOT_HARNESSES="$(needle_declared_test_harnesses)"
if [[ "$GOT_HARNESSES" == "$WANT_HARNESSES" ]]; then
  ok "Clippy reads the five declared test harness roots from Cargo.toml"
else
  bad "declared test harness parsing drifted (got: $(echo "$GOT_HARNESSES" | tr '\n' ' '))"
fi

LANE="fast"
CHANGED_ONLY=false
COMPREHENSIVE_LINT=false
STAGED_PATHS=()
GOT_CLIPPY="$(needle_clippy_selectors | tr '\n' ' ' | sed 's/ $//')"
if [[ "$GOT_CLIPPY" == "--lib --bins" ]]; then
  ok "push/gate fast lint selects only product lib and bins by default"
else
  bad "push/gate fast lint selectors drifted (got: $GOT_CLIPPY)"
fi

CHANGED_ONLY=true
STAGED_PATHS=(
  src/worker/mod.rs
  tests/integration_tests/config_key_path_integration.rs
  tests/integration_tests/edge_case_panic_tests.rs
  tests/p2_integration_tests.rs
  tests/p2_integration_tests/cli_bead_store_engine.rs
  docs/definition-of-done.md
)
GOT_CLIPPY="$(needle_clippy_selectors | tr '\n' ' ' | sed 's/ $//')"
if [[ "$GOT_CLIPPY" == "--lib --bins --test integration_tests --test p2_integration_tests" ]]; then
  ok "changed test roots/modules add each affected declared harness once"
else
  bad "affected test harness selectors drifted (got: $GOT_CLIPPY)"
fi

STAGED_PATHS=(tests/integration_spawn.rs)
GOT_CLIPPY="$(needle_clippy_selectors | tr '\n' ' ' | sed 's/ $//')"
if [[ "$GOT_CLIPPY" == "--lib --bins --test integration_spawn" ]]; then
  ok "a changed standalone declared test harness is linted"
else
  bad "standalone test harness selector drifted (got: $GOT_CLIPPY)"
fi

LANE="all"
CHANGED_ONLY=true
COMPREHENSIVE_LINT=false
STAGED_PATHS=(tests/integration_tests/edge_case_panic_tests.rs)
GOT_CLIPPY="$(needle_clippy_selectors | tr '\n' ' ' | sed 's/ $//')"
if [[ "$GOT_CLIPPY" == "--all-targets" ]]; then
  ok "all/scheduled lint remains comprehensive"
else
  bad "all/scheduled lint no longer uses --all-targets (got: $GOT_CLIPPY)"
fi

LANE="fast"
CHANGED_ONLY=false
COMPREHENSIVE_LINT=true
STAGED_PATHS=()
GOT_CLIPPY="$(needle_clippy_selectors | tr '\n' ' ' | sed 's/ $//')"
if [[ "$GOT_CLIPPY" == "--all-targets" ]]; then
  ok "periodic fast lane can request comprehensive lint without running tests"
else
  bad "periodic comprehensive lint selector drifted (got: $GOT_CLIPPY)"
fi

run_check() {
  local name="$1"
  shift
  printf '%s|%s\n' "$name" "$*"
}
LANE="all"
CHANGED_ONLY=true
COMPREHENSIVE_LINT=false
STAGED_PATHS=(tests/integration_tests/edge_case_panic_tests.rs)
GOT_CLIPPY_CALL="$(needle_run_clippy)"
if [[ "$GOT_CLIPPY_CALL" == "cargo clippy|cargo clippy --all-targets --message-format short -- -D warnings" ]]; then
  ok "all/scheduled Clippy command retains zero-warning enforcement"
else
  bad "all/scheduled Clippy command drifted (got: $GOT_CLIPPY_CALL)"
fi

LANE="fast"
CHANGED_ONLY=false
COMPREHENSIVE_LINT=true
STAGED_PATHS=()
GOT_CLIPPY_CALL="$(needle_run_clippy)"
if [[ "$GOT_CLIPPY_CALL" == "cargo clippy|cargo clippy --all-targets -- -D warnings" ]]; then
  ok "periodic comprehensive Clippy command retains zero-warning enforcement"
else
  bad "periodic comprehensive Clippy command drifted (got: $GOT_CLIPPY_CALL)"
fi

LANE="fast"
CHANGED_ONLY=false
COMPREHENSIVE_LINT=false
STAGED_PATHS=()
GOT_CLIPPY_CALL="$(needle_run_clippy)"
if [[ "$GOT_CLIPPY_CALL" == "cargo clippy|cargo clippy --lib --bins -- -D warnings" ]]; then
  ok "fast Clippy command retains zero-warning enforcement"
else
  bad "fast Clippy command drifted (got: $GOT_CLIPPY_CALL)"
fi

# ── argument parsing: both --target forms must reach target validation ───────
# needle-workflowtemplate.yml passes the equals form
# (`--target={{inputs.parameters.target}}`). A parser that accepted only the
# space form let every slow-lane pod die on "Unknown argument" while the fast
# lane stayed green (2026-09-05), so the forms themselves are tested here —
# against the real script, since the parser is inline rather than extracted.
# An unknown name keeps the run cheap: the script rejects it before any cargo
# or git work happens.
for form in "--target=nope" "--target nope"; do
  # shellcheck disable=SC2086 # deliberate word split for the space form
  if OUT="$(bash "$DOD" --slow $form 2>&1)"; then
    bad "$form should fail on an unknown target, not run"
  elif grep -q "Unknown argument" <<<"$OUT"; then
    bad "$form must parse: 'Unknown argument' means the form was not accepted"
  else
    ok "$form parses and is rejected for an unknown target"
  fi
done

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

# ── Hermetic slow-lane context ───────────────────────────────────────────────
# lane_tmp must mutate the parent shell. Calling it through $(...) would create
# the directory but lose the cleanup registration when the subshell exits.
DOD_TMP_ROOT="$test_tmp_root"
LANE_TMPS=()
allocated_tmp=""
lane_tmp regression allocated_tmp
if [[ -d "$allocated_tmp" && "${#LANE_TMPS[@]}" -eq 1 && "${LANE_TMPS[0]}" == "$allocated_tmp" ]]; then
  ok "lane_tmp returns through a variable and registers cleanup in the parent shell"
else
  bad "lane_tmp did not register the allocated directory in the parent shell"
fi
POISON_WE_CREATED=0
cleanup_lane_tmps
if [[ ! -e "$allocated_tmp" ]]; then ok "registered lane directories are cleaned"
else bad "registered lane directory survived cleanup"; fi

# Both build and run pass through one wrapper, preventing their build-context
# inputs from drifting independently.
SLOW_TMPDIR="$test_tmp_root/context-tmp"
SLOW_CARGO_TARGET_DIR="$test_tmp_root/context-target"
run_check() {
  printf '%s|%s|%s\n' "$DOD_TIMING_PHASE" "$DOD_TIMING_TARGET" "$*"
}
BUILD_CALL="$(run_slow_cargo_check build lib build-check cargo test --no-run --lib)"
RUN_CALL="$(run_slow_cargo_check run lib run-check cargo test --lib)"
for call in "$BUILD_CALL" "$RUN_CALL"; do
  if [[ "$call" == *"env TMPDIR=$SLOW_TMPDIR CARGO_TARGET_DIR=$SLOW_CARGO_TARGET_DIR"* ]]; then
    ok "slow Cargo invocation uses the shared TMPDIR and CARGO_TARGET_DIR"
  else
    bad "slow Cargo invocation escaped the shared context (got: $call)"
  fi
done
if [[ "$BUILD_CALL" == build\|lib\|* && "$RUN_CALL" == run\|lib\|* ]]; then
  ok "slow Cargo wrapper labels build and run timing phases"
else
  bad "slow Cargo wrapper did not label timing phases"
fi

WANT_EVENT='NEEDLE_DOD_TIMING {"phase":"build","target":"lib","duration_ms":123,"status":"pass","exit_code":0}'
GOT_EVENT="$(needle_duration_event build lib 123 pass 0)"
if [[ "$GOT_EVENT" == "$WANT_EVENT" ]]; then ok "duration event is structured and machine-readable"
else bad "duration event format drifted (got: $GOT_EVENT)"; fi

echo ""
echo "passed: $pass  failed: $fail"
[[ "$fail" -eq 0 ]]
