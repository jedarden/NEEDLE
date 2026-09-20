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

for fn in needle_now_ms needle_duration_event reap_orphans run_check lane_tmp cleanup_lane_tmps run_slow_cargo_check needle_validate_nextest_release run_slow_nextest_check needle_slow_targets needle_cargo_selector needle_nextest_filter needle_validate_archive_mode selected_cargo_targets needle_declared_test_harnesses needle_expected_slow_targets needle_validate_slow_target_coverage needle_affected_test_targets needle_clippy_selectors needle_run_clippy needle_gate_skips_slow_lane; do
  awk -v f="^${fn}\\\\(\\\\)" '$0 ~ f, /^}/' "$DOD" >> "$extracted"
done
# Every function must have been found, or the test would silently pass.
for fn in needle_now_ms needle_duration_event reap_orphans run_check lane_tmp cleanup_lane_tmps run_slow_cargo_check needle_validate_nextest_release run_slow_nextest_check needle_slow_targets needle_cargo_selector needle_nextest_filter needle_validate_archive_mode selected_cargo_targets needle_declared_test_harnesses needle_expected_slow_targets needle_validate_slow_target_coverage needle_affected_test_targets needle_clippy_selectors needle_run_clippy needle_gate_skips_slow_lane; do
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

# assert_nextest_filter <desc> <expected-filter> <name>
assert_nextest_filter() {
  local desc="$1" want="$2" name="$3" got
  got="$(needle_nextest_filter "$name" 2>/dev/null)"
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
assert_lines "target table lists 26 names" 26 needle_slow_targets

# Exact set, asserted literally: a renamed or dropped target must be caught
# here rather than silently accepted by every consumer of the table.
WANT_TABLE="$(printf '%s\n' lib integration_spawn integration_tests p2_integration_tests p3_integration_tests real_br_integration_tests escalation_ladder nt45_failure_evidence_capture nt50_exception_lessons nt51_adapter_usage_capture nt10_policy_precedence nt10_policy_hashing nt10_context_manifest nt51_gateway_health nt52_state_dir_isolation nt07_admission_policy nt07_admission_budgets nt53_improvement_proposals nt54_proposal_admission nt55_impact_receipts nt56_improvements_cli nt07_proposal_contract nt07_impact_contract nt07_impact_scoring nt07_executable_admission installer)"
GOT_TABLE="$(needle_slow_targets)"
if [[ "$GOT_TABLE" == "$WANT_TABLE" ]]; then
  ok "target table is exactly the 25 cargo targets plus installer"
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
assert_selector "escalation ladder selects its target" "--test escalation_ladder" escalation_ladder
assert_selector "N-T10 selects its target" "--test nt10_policy_precedence" nt10_policy_precedence
assert_selector "N-T10 canonical hashing selects its target" "--test nt10_policy_hashing" nt10_policy_hashing
assert_selector "N-T10 ContextManifest selects its target" "--test nt10_context_manifest" nt10_context_manifest
assert_selector "N-T45 selects its target" "--test nt45_failure_evidence_capture" nt45_failure_evidence_capture
assert_selector "N-T50 selects its target" "--test nt50_exception_lessons" nt50_exception_lessons
assert_selector "N-T51 usage selects its target" "--test nt51_adapter_usage_capture" nt51_adapter_usage_capture
assert_selector "N-T51 selects its target" "--test nt51_gateway_health" nt51_gateway_health
assert_selector "N-T52 selects its target" "--test nt52_state_dir_isolation" nt52_state_dir_isolation
assert_fails "unknown target has no selector" needle_cargo_selector nope
assert_fails "installer is not a cargo target" needle_cargo_selector installer

# ── needle_nextest_filter ────────────────────────────────────────────────────
assert_nextest_filter "lib has an exact nextest binary ID" "binary_id(=needle)" lib
assert_nextest_filter "integration_spawn has an exact nextest binary ID" "binary_id(=needle::integration_spawn)" integration_spawn
assert_nextest_filter "integration_tests has an exact nextest binary ID" "binary_id(=needle::integration_tests)" integration_tests
assert_nextest_filter "p2 has an exact nextest binary ID" "binary_id(=needle::p2_integration_tests)" p2_integration_tests
assert_nextest_filter "p3 has an exact nextest binary ID" "binary_id(=needle::p3_integration_tests)" p3_integration_tests
assert_nextest_filter "real_br has an exact nextest binary ID" "binary_id(=needle::real_br_integration_tests)" real_br_integration_tests
assert_nextest_filter "N-T10 has an exact nextest binary ID" "binary_id(=needle::nt10_policy_precedence)" nt10_policy_precedence
assert_nextest_filter "N-T10 hashing has an exact nextest binary ID" "binary_id(=needle::nt10_policy_hashing)" nt10_policy_hashing
assert_nextest_filter "N-T10 ContextManifest has an exact nextest binary ID" "binary_id(=needle::nt10_context_manifest)" nt10_context_manifest
assert_nextest_filter "escalation ladder has an exact nextest binary ID" "binary_id(=needle::escalation_ladder)" escalation_ladder
assert_nextest_filter "N-T45 has an exact nextest binary ID" "binary_id(=needle::nt45_failure_evidence_capture)" nt45_failure_evidence_capture
assert_nextest_filter "N-T50 has an exact nextest binary ID" "binary_id(=needle::nt50_exception_lessons)" nt50_exception_lessons
assert_nextest_filter "N-T51 usage has an exact nextest binary ID" "binary_id(=needle::nt51_adapter_usage_capture)" nt51_adapter_usage_capture
assert_nextest_filter "N-T51 has an exact nextest binary ID" "binary_id(=needle::nt51_gateway_health)" nt51_gateway_health
assert_nextest_filter "N-T52 has an exact nextest binary ID" "binary_id(=needle::nt52_state_dir_isolation)" nt52_state_dir_isolation
assert_fails "unknown target has no nextest filter" needle_nextest_filter nope
assert_fails "installer has no nextest filter" needle_nextest_filter installer

# ── selected_cargo_targets ───────────────────────────────────────────────────
SLOW_TARGET=""
assert_lines "default selection is all 25 cargo targets" 25 selected_cargo_targets

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
    printf '%s
' lib \
      integration_tests \
      p2_integration_tests \
      p3_integration_tests \
      real_br_integration_tests \
      escalation_ladder \
      nt45_failure_evidence_capture \
      nt50_exception_lessons \
      nt51_adapter_usage_capture \
      nt51_gateway_health \
      nt52_state_dir_isolation \
      nt07_admission_policy \
      nt07_admission_budgets \
      nt53_improvement_proposals \
      nt07_proposal_contract \
      nt07_impact_contract \
      nt07_impact_scoring \
      nt07_executable_admission \
      installer
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

# ── archive-mode validation ──────────────────────────────────────────────────
real_repo_root="$REPO_ROOT"
archive_workspace="$test_tmp_root/archive-workspace"
valid_archive="$test_tmp_root/needle-nextest.tar.zst"
empty_archive="$test_tmp_root/empty.tar.zst"
mkdir -p "$archive_workspace"
printf 'archive fixture\n' > "$valid_archive"
: > "$empty_archive"
REPO_ROOT="$archive_workspace"

ARCHIVE_FILE=""
ARCHIVE_FILE_ARGUMENT_COUNT=0
SLOW_TARGET=""
SLOW_TARGET_ARGUMENT_COUNT=0
if needle_validate_archive_mode; then ok "ordinary non-archive mode remains valid"
else bad "ordinary non-archive mode was rejected"; fi

ARCHIVE_FILE="$valid_archive"
ARCHIVE_FILE_ARGUMENT_COUNT=1
SLOW_TARGET=""
SLOW_TARGET_ARGUMENT_COUNT=0
assert_fails "archive mode rejects aggregate execution" needle_validate_archive_mode

SLOW_TARGET="installer"
SLOW_TARGET_ARGUMENT_COUNT=1
assert_fails "archive mode rejects installer" needle_validate_archive_mode

SLOW_TARGET="lib"
SLOW_TARGET_ARGUMENT_COUNT=2
assert_fails "archive mode rejects repeated --target arguments" needle_validate_archive_mode

SLOW_TARGET_ARGUMENT_COUNT=1
ARCHIVE_FILE_ARGUMENT_COUNT=2
assert_fails "archive mode rejects repeated --archive-file arguments" needle_validate_archive_mode

ARCHIVE_FILE_ARGUMENT_COUNT=1
ARCHIVE_FILE="needle-nextest.tar.zst"
assert_fails "archive mode rejects a non-absolute archive path" needle_validate_archive_mode

ARCHIVE_FILE="$empty_archive"
assert_fails "archive mode rejects an empty archive" needle_validate_archive_mode

ARCHIVE_FILE="$test_tmp_root/wrong-extension.tar.gz"
printf 'archive fixture\n' > "$ARCHIVE_FILE"
assert_fails "archive mode requires the tar.zst archive contract" needle_validate_archive_mode

ARCHIVE_FILE="$valid_archive"
if needle_validate_archive_mode; then ok "archive mode accepts one exact Cargo target and a nonempty archive"
else bad "valid archive mode was rejected"; fi

mkdir "$archive_workspace/target"
assert_fails "archive mode refuses an existing extraction target" needle_validate_archive_mode
rmdir "$archive_workspace/target"

REPO_ROOT="$real_repo_root"
ARCHIVE_FILE=""
ARCHIVE_FILE_ARGUMENT_COUNT=0
SLOW_TARGET=""
SLOW_TARGET_ARGUMENT_COUNT=0

# ── fast-lane Clippy target selection ────────────────────────────────────────
WANT_HARNESSES="$(printf '%s\t%s\n' \
  integration_spawn tests/integration_spawn.rs \
  integration_tests tests/integration_tests.rs \
  p2_integration_tests tests/p2_integration_tests.rs \
  p3_integration_tests tests/p3_integration_tests.rs \
  real_br_integration_tests tests/real_br_integration_tests.rs \
  escalation_ladder tests/escalation_ladder.rs \
  nt45_failure_evidence_capture tests/nt45_failure_evidence_capture.rs \
  nt50_exception_lessons tests/nt50_exception_lessons.rs \
  nt51_adapter_usage_capture tests/nt51_adapter_usage_capture.rs \
  nt10_policy_precedence tests/nt10_policy_precedence.rs \
  nt10_policy_hashing tests/nt10_policy_hashing.rs \
  nt10_context_manifest tests/nt10_context_manifest.rs \
  nt51_gateway_health tests/nt51_gateway_health.rs \
  nt52_state_dir_isolation tests/nt52_state_dir_isolation.rs \
  nt07_admission_policy tests/nt07_admission_policy.rs \
  nt07_admission_budgets tests/nt07_admission_budgets.rs \
  nt53_improvement_proposals tests/nt53_improvement_proposals.rs \
  nt54_proposal_admission tests/nt54_proposal_admission.rs \
  nt55_impact_receipts tests/nt55_impact_receipts.rs \
  nt56_improvements_cli tests/nt56_improvements_cli.rs \
  nt07_proposal_contract tests/nt07_proposal_contract.rs \
  nt07_impact_contract tests/nt07_impact_contract.rs \
  nt07_impact_scoring tests/nt07_impact_scoring.rs \
  nt07_executable_admission tests/nt07_executable_admission.rs)"
GOT_HARNESSES="$(needle_declared_test_harnesses)"
if [[ "$GOT_HARNESSES" == "$WANT_HARNESSES" ]]; then
  ok "Clippy reads the 23 declared test harness roots from Cargo.toml"
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

for form in "--archive-file=$valid_archive" "--archive-file $valid_archive"; do
  # shellcheck disable=SC2086 # deliberate word split for the space form
  if OUT="$(bash "$DOD" --slow $form 2>&1)"; then
    bad "$form should require one target, not run"
  elif grep -q "Unknown argument" <<<"$OUT"; then
    bad "$form must parse: 'Unknown argument' means the form was not accepted"
  elif grep -q "requires exactly one --target" <<<"$OUT"; then
    ok "$form parses and requires one target"
  else
    bad "$form failed for an unexpected reason"
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

ARCHIVE_FILE="$test_tmp_root/needle-nextest.tar.zst"
REPO_ROOT="$real_repo_root"
nextest_shim_dir="$test_tmp_root/nextest-shim"
mkdir -p "$nextest_shim_dir"
cat > "$nextest_shim_dir/cargo-nextest" <<'SHIM'
#!/usr/bin/env bash
set -eu
[[ "$#" -eq 1 && "$1" == "--version" ]] || exit 64
case "${NEXTEST_VERSION_SHIM_MODE:-missing}" in
  exact)
    printf '%s\n' 'cargo-nextest 0.9.144' 'release: 0.9.144' 'commit hash: fixture'
    ;;
  wrong)
    printf '%s\n' 'cargo-nextest 0.9.145' 'release: 0.9.145' 'commit hash: fixture'
    ;;
  multiple)
    printf '%s\n' 'cargo-nextest 0.9.144' 'release: 0.9.144' 'release: 0.9.144'
    ;;
  missing)
    printf '%s\n' 'cargo-nextest 0.9.144' 'commit hash: fixture'
    ;;
  *)
    exit 65
    ;;
esac
SHIM
chmod +x "$nextest_shim_dir/cargo-nextest"
PATH="$nextest_shim_dir:$PATH"
export PATH NEXTEST_VERSION_SHIM_MODE

for NEXTEST_VERSION_SHIM_MODE in wrong missing multiple; do
  assert_fails "archive runner rejects $NEXTEST_VERSION_SHIM_MODE cargo-nextest release output" \
    run_slow_nextest_check lib 'binary_id(=needle)'
done

NEXTEST_VERSION_SHIM_MODE=exact
NEXTEST_CALL=""
if NEXTEST_CALL="$(run_slow_nextest_check lib 'binary_id(=needle)')"; then
  ok "archive runner accepts exactly one pinned cargo-nextest release field"
else
  bad "archive runner rejected the exact pinned cargo-nextest release field"
fi
if [[ "$NEXTEST_CALL" == run\|lib\|*"env -u CARGO_TARGET_DIR TMPDIR=$SLOW_TMPDIR timeout --kill-after=30 900 cargo-nextest nextest run"* \
  && "$NEXTEST_CALL" == *"--archive-file $ARCHIVE_FILE --extract-to $REPO_ROOT --profile ci"* \
  && "$NEXTEST_CALL" == *"--run-ignored default --ignore-default-filter --no-fail-fast --no-tests fail -E binary_id(=needle)"* ]]; then
  ok "archive runner preserves the slow-lane deadline and exact nextest filter"
else
  bad "archive runner command drifted (got: $NEXTEST_CALL)"
fi
if [[ "$NEXTEST_CALL" == *"env -u CARGO_TARGET_DIR"* \
  && "$NEXTEST_CALL" != *"cargo nextest"* \
  && "$NEXTEST_CALL" != *"cargo test"* \
  && "$NEXTEST_CALL" != *" --lib"* \
  && "$NEXTEST_CALL" != *" --test"* ]]; then
  ok "archive runner clears inherited target configuration and cannot compile or pass conflicting Cargo target selectors"
else
  bad "archive runner reintroduced Cargo dispatch, a build context, or a Cargo target selector"
fi

WANT_EVENT='NEEDLE_DOD_TIMING {"phase":"build","target":"lib","duration_ms":123,"status":"pass","exit_code":0}'
GOT_EVENT="$(needle_duration_event build lib 123 pass 0)"
if [[ "$GOT_EVENT" == "$WANT_EVENT" ]]; then ok "duration event is structured and machine-readable"
else bad "duration event format drifted (got: $GOT_EVENT)"; fi

echo ""
echo "passed: $pass  failed: $fail"
[[ "$fail" -eq 0 ]]
