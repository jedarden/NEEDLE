#!/usr/bin/env bash
# Tests for NEEDLE post-execute file conflict reconciliation
# (src/hooks/post-execute-reconcile.sh)

set -euo pipefail

# Test setup
TEST_DIR=$(mktemp -d)
TEST_LOCK_DIR="$TEST_DIR/needle-locks"
TEST_WORKSPACE="$TEST_DIR/workspace"

# Initialise a real git repo in the test workspace
mkdir -p "$TEST_WORKSPACE"
git -C "$TEST_WORKSPACE" init -q
git -C "$TEST_WORKSPACE" config user.email "test@needle"
git -C "$TEST_WORKSPACE" config user.name  "Test"

# Create an initial commit so HEAD exists
echo "original content" > "$TEST_WORKSPACE/file_a.sh"
echo "original content" > "$TEST_WORKSPACE/file_b.sh"
echo "original content" > "$TEST_WORKSPACE/file_c.sh"
git -C "$TEST_WORKSPACE" add .
git -C "$TEST_WORKSPACE" commit -q -m "initial"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Stub telemetry
_needle_telemetry_emit() { return 0; }

# Source dependencies
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/json.sh"

# Load lock module with test lock dir
export NEEDLE_LOCK_DIR="$TEST_LOCK_DIR"
mkdir -p "$TEST_LOCK_DIR"
source "$PROJECT_DIR/src/lock/checkout.sh"
source "$PROJECT_DIR/src/lock/metrics.sh"

# Set environment for the reconcile module
export NEEDLE_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false
export NEEDLE_LOG_INITIALIZED=true
export NEEDLE_SESSION="test-session-reconcile"

# Override NEEDLE_LOCK_MODULE / NEEDLE_METRICS_MODULE so the reconcile script
# re-uses the already-loaded functions (they're already in scope)
export NEEDLE_LOCK_MODULE="/dev/null"
export NEEDLE_METRICS_MODULE="/dev/null"

# Source the module under test
source "$PROJECT_DIR/src/hooks/post-execute-reconcile.sh"

# Cleanup
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

test_case() {
    local name="$1"
    TESTS_RUN=$((TESTS_RUN + 1))
    echo -n "Testing: $name... "
}

test_pass() {
    echo "PASS"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

reset_locks() {
    rm -rf "$TEST_LOCK_DIR"
    mkdir -p "$TEST_LOCK_DIR"
}

# Reset workspace file to HEAD state and make a dirty modification
make_change() {
    local file="$1"
    echo "modified content $(date +%s)" > "$TEST_WORKSPACE/$file"
}

reset_file() {
    local file="$1"
    git -C "$TEST_WORKSPACE" checkout HEAD -- "$file" 2>/dev/null || true
}

# ============================================================================
# Tests
# ============================================================================

echo "=== Post-Execute Reconciliation Tests ==="
echo ""

# Test 1: No conflicts when no files changed
test_case "detect_file_conflicts: returns 0 with no changes"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t1"
if detect_file_conflicts 2>/dev/null; then
    test_pass
else
    test_fail "Expected 0 (no conflicts) when working tree is clean"
fi

# Test 2: No conflict when changed file is not locked
test_case "detect_file_conflicts: no conflict when file is not locked"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t2"
make_change "file_a.sh"
if detect_file_conflicts 2>/dev/null; then
    test_pass
else
    test_fail "Expected 0 when changed file has no lock"
fi
reset_file "file_a.sh"

# Test 3: No conflict when file is locked by current bead
test_case "detect_file_conflicts: no conflict when locked by own bead"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t3"
checkout_file "$TEST_WORKSPACE/file_a.sh" "nd-reconcile-t3" "worker-own" 2>/dev/null
make_change "file_a.sh"
if detect_file_conflicts 2>/dev/null; then
    test_pass
else
    test_fail "Expected 0 when file is locked by current bead"
fi
reset_file "file_a.sh"
reset_locks

# Test 4: Conflict detected when file locked by different bead
test_case "detect_file_conflicts: detects conflict when locked by other bead"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t4"
checkout_file "$TEST_WORKSPACE/file_b.sh" "nd-other-bead" "worker-other" 2>/dev/null
make_change "file_b.sh"
conflict_result=0
detect_file_conflicts 2>/dev/null || conflict_result=$?
if [[ $conflict_result -eq 1 ]]; then
    test_pass
else
    test_fail "Expected 1 (conflict detected), got $conflict_result"
fi
reset_file "file_b.sh"
reset_locks

# Test 5: Conflicting change is rolled back
test_case "detect_file_conflicts: rolls back conflicting change"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t5"
checkout_file "$TEST_WORKSPACE/file_b.sh" "nd-blocker" "worker-blocker" 2>/dev/null
make_change "file_b.sh"
detect_file_conflicts 2>/dev/null || true
# After rollback, diff should be empty for that file
dirty=$(git -C "$TEST_WORKSPACE" diff HEAD -- file_b.sh 2>/dev/null || true)
if [[ -z "$dirty" ]]; then
    test_pass
else
    test_fail "Expected file_b.sh to be rolled back to HEAD"
fi
reset_locks

# Test 6: Non-conflicting file is NOT rolled back
test_case "detect_file_conflicts: preserves non-conflicting changed files"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t6"
# Lock file_b by another bead, change file_a (no lock) and file_b (locked)
checkout_file "$TEST_WORKSPACE/file_b.sh" "nd-another" "worker-another" 2>/dev/null
make_change "file_a.sh"
make_change "file_b.sh"
detect_file_conflicts 2>/dev/null || true
# file_a should remain dirty (not rolled back)
dirty_a=$(git -C "$TEST_WORKSPACE" diff HEAD -- file_a.sh 2>/dev/null || true)
if [[ -n "$dirty_a" ]]; then
    test_pass
else
    test_fail "Expected file_a.sh to remain changed (no lock conflict)"
fi
reset_file "file_a.sh"
reset_file "file_b.sh"
reset_locks

# Test 7: Multiple conflicts all rolled back
test_case "detect_file_conflicts: rolls back multiple conflicts"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t7"
checkout_file "$TEST_WORKSPACE/file_a.sh" "nd-locker-a" "worker-a" 2>/dev/null
checkout_file "$TEST_WORKSPACE/file_b.sh" "nd-locker-b" "worker-b" 2>/dev/null
make_change "file_a.sh"
make_change "file_b.sh"
detect_file_conflicts 2>/dev/null || true
dirty_a=$(git -C "$TEST_WORKSPACE" diff HEAD -- file_a.sh 2>/dev/null || true)
dirty_b=$(git -C "$TEST_WORKSPACE" diff HEAD -- file_b.sh 2>/dev/null || true)
if [[ -z "$dirty_a" ]] && [[ -z "$dirty_b" ]]; then
    test_pass
else
    test_fail "Expected both files rolled back. dirty_a='$dirty_a' dirty_b='$dirty_b'"
fi
reset_locks

# Test 8: Conflict emits metric event
test_case "detect_file_conflicts: emits conflict.missed metric event"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t8"
# Use a fresh metrics file
TEST_METRICS_DIR="$TEST_DIR/metrics-t8"
mkdir -p "$TEST_METRICS_DIR"
export NEEDLE_METRICS_DIR="$TEST_METRICS_DIR"
NEEDLE_COLLISION_EVENTS="$TEST_METRICS_DIR/collision_events.jsonl"
checkout_file "$TEST_WORKSPACE/file_c.sh" "nd-metric-blocker" "worker-mb" 2>/dev/null
make_change "file_c.sh"
detect_file_conflicts 2>/dev/null || true
if [[ -f "$NEEDLE_COLLISION_EVENTS" ]] && grep -q "conflict.missed" "$NEEDLE_COLLISION_EVENTS" 2>/dev/null; then
    test_pass
else
    test_fail "Expected conflict.missed event in $NEEDLE_COLLISION_EVENTS"
fi
reset_file "file_c.sh"
reset_locks
unset NEEDLE_METRICS_DIR

# Test 9: No bead ID set — silently skips
test_case "detect_file_conflicts: no-op when NEEDLE_BEAD_ID is unset"
reset_locks
unset NEEDLE_BEAD_ID
make_change "file_a.sh"
if detect_file_conflicts 2>/dev/null; then
    test_pass
else
    test_fail "Expected 0 (no-op) when NEEDLE_BEAD_ID is not set"
fi
export NEEDLE_BEAD_ID="nd-default"
reset_file "file_a.sh"
reset_locks

# Test 10: Workspace not a git repo — silently skips
test_case "detect_file_conflicts: no-op when workspace is not a git repo"
reset_locks
export NEEDLE_BEAD_ID="nd-reconcile-t10"
ORIG_WORKSPACE="$NEEDLE_WORKSPACE"
export NEEDLE_WORKSPACE="$TEST_DIR/not-a-repo"
mkdir -p "$NEEDLE_WORKSPACE"
if detect_file_conflicts 2>/dev/null; then
    test_pass
else
    test_fail "Expected 0 (no-op) when workspace is not a git repo"
fi
export NEEDLE_WORKSPACE="$ORIG_WORKSPACE"
reset_locks

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=== Results ==="
echo "Passed: $TESTS_PASSED / $TESTS_RUN"
echo "Failed: $TESTS_FAILED / $TESTS_RUN"
echo ""

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi
exit 0
