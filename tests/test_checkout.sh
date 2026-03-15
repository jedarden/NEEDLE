#!/usr/bin/env bash
# Tests for NEEDLE file checkout system (src/lock/checkout.sh)

set -euo pipefail

# Test setup
TEST_DIR=$(mktemp -d)
TEST_LOCK_DIR="$TEST_DIR/needle-locks"

# Source the module
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_LOCK_DIR="$TEST_LOCK_DIR"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false
export NEEDLE_LOG_INITIALIZED=true
export NEEDLE_SESSION="test-session-checkout"

# Source required modules
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lock/checkout.sh"

# Stub telemetry to avoid side effects
_needle_telemetry_emit() { return 0; }

# Stub worker alive check: test worker IDs don't map to real tmux sessions
_needle_lock_worker_alive() { return 0; }

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

# Helper: reset lock dir between tests
reset_locks() {
    rm -rf "$TEST_LOCK_DIR"
    mkdir -p "$TEST_LOCK_DIR"
}

# ============================================================================
# Tests
# ============================================================================

echo "=== File Checkout System Tests ==="
echo ""

# Test 1: _needle_lock_path_uuid generates consistent 8-char hex
test_case "_needle_lock_path_uuid returns 8-char hex"
uuid=$(_needle_lock_path_uuid "/some/file.sh")
if [[ ${#uuid} -eq 8 ]] && [[ "$uuid" =~ ^[0-9a-f]{8}$ ]]; then
    test_pass
else
    test_fail "Expected 8-char hex, got: '$uuid'"
fi

# Test 2: Same path produces same UUID
test_case "_needle_lock_path_uuid is deterministic"
uuid1=$(_needle_lock_path_uuid "/home/coder/NEEDLE/src/cli/run.sh")
uuid2=$(_needle_lock_path_uuid "/home/coder/NEEDLE/src/cli/run.sh")
if [[ "$uuid1" == "$uuid2" ]]; then
    test_pass
else
    test_fail "UUIDs differ: $uuid1 vs $uuid2"
fi

# Test 3: Different paths produce different UUIDs
test_case "_needle_lock_path_uuid differs for different paths"
uuid_a=$(_needle_lock_path_uuid "/home/coder/NEEDLE/src/cli/run.sh")
uuid_b=$(_needle_lock_path_uuid "/home/coder/NEEDLE/src/cli/stop.sh")
if [[ "$uuid_a" != "$uuid_b" ]]; then
    test_pass
else
    test_fail "UUIDs should differ for different paths"
fi

# Test 4: checkout_file acquires lock on free file
test_case "checkout_file acquires lock on free file"
reset_locks
if checkout_file "/tmp/test-file-a.sh" "nd-test1" "worker-1" 2>/dev/null; then
    test_pass
else
    test_fail "Expected checkout to succeed"
fi

# Test 5: Lock file is created
test_case "checkout_file creates lock file"
reset_locks
checkout_file "/tmp/test-file-b.sh" "nd-test2" "worker-1" 2>/dev/null
uuid=$(_needle_lock_path_uuid "/tmp/test-file-b.sh")
lock_file="$TEST_LOCK_DIR/nd-test2-$uuid"
if [[ -f "$lock_file" ]]; then
    test_pass
else
    test_fail "Lock file not found: $lock_file"
fi

# Test 6: Lock file contains correct JSON
test_case "checkout_file writes correct lock info JSON"
reset_locks
checkout_file "/tmp/test-file-c.sh" "nd-test3" "worker-abc" 2>/dev/null
uuid=$(_needle_lock_path_uuid "/tmp/test-file-c.sh")
lock_file="$TEST_LOCK_DIR/nd-test3-$uuid"
if [[ -f "$lock_file" ]]; then
    bead=$(jq -r '.bead' "$lock_file" 2>/dev/null)
    worker=$(jq -r '.worker' "$lock_file" 2>/dev/null)
    ltype=$(jq -r '.type' "$lock_file" 2>/dev/null)
    if [[ "$bead" == "nd-test3" ]] && [[ "$worker" == "worker-abc" ]] && [[ "$ltype" == "write" ]]; then
        test_pass
    else
        test_fail "JSON contents wrong: bead=$bead worker=$worker type=$ltype"
    fi
else
    test_fail "Lock file not found"
fi

# Test 7: checkout_file fails when another bead holds the lock
test_case "checkout_file fails when file locked by another bead"
reset_locks
checkout_file "/tmp/test-conflict.sh" "nd-owner" "worker-1" >/dev/null 2>&1
checkout_file "/tmp/test-conflict.sh" "nd-other" "worker-2" >/dev/null 2>&1 && status=0 || status=$?
if [[ $status -eq 1 ]]; then
    test_pass
else
    test_fail "Expected status 1, got $status"
fi

# Test 8: checkout_file conflict output includes blocking bead info
test_case "checkout_file conflict output contains blocking bead"
reset_locks
checkout_file "/tmp/test-conflict2.sh" "nd-blocker" "worker-1" 2>/dev/null
blocking_info=$(checkout_file "/tmp/test-conflict2.sh" "nd-waiter" "worker-2" 2>&1 >/dev/null) || true
blocking_bead=$(echo "$blocking_info" | jq -r '.bead' 2>/dev/null || true)
if [[ "$blocking_bead" == "nd-blocker" ]]; then
    test_pass
else
    test_fail "Expected blocking bead 'nd-blocker', got '$blocking_bead'"
fi

# Test 9: checkout_file allows same bead to re-checkout (idempotent)
test_case "checkout_file allows same bead to re-checkout same file"
reset_locks
checkout_file "/tmp/test-recheck.sh" "nd-same" "worker-1" 2>/dev/null
if checkout_file "/tmp/test-recheck.sh" "nd-same" "worker-1" 2>/dev/null; then
    test_pass
else
    test_fail "Expected re-checkout by same bead to succeed"
fi

# Test 10: release_file removes lock
test_case "release_file removes lock"
reset_locks
checkout_file "/tmp/test-release.sh" "nd-rel1" "worker-1" 2>/dev/null
release_file "/tmp/test-release.sh" "nd-rel1" 2>/dev/null
uuid=$(_needle_lock_path_uuid "/tmp/test-release.sh")
lock_file="$TEST_LOCK_DIR/nd-rel1-$uuid"
if [[ ! -f "$lock_file" ]]; then
    test_pass
else
    test_fail "Lock file should be removed after release"
fi

# Test 11: After release, another bead can acquire
test_case "After release_file, another bead can acquire lock"
reset_locks
checkout_file "/tmp/test-release2.sh" "nd-first" "worker-1" 2>/dev/null
release_file "/tmp/test-release2.sh" "nd-first" 2>/dev/null
if checkout_file "/tmp/test-release2.sh" "nd-second" "worker-2" 2>/dev/null; then
    test_pass
else
    test_fail "Expected second checkout to succeed after release"
fi

# Test 12: release_bead_locks releases all locks for a bead
test_case "release_bead_locks releases all locks for a bead"
reset_locks
checkout_file "/tmp/test-bulk1.sh" "nd-bulk" "worker-1" 2>/dev/null
checkout_file "/tmp/test-bulk2.sh" "nd-bulk" "worker-1" 2>/dev/null
checkout_file "/tmp/test-bulk3.sh" "nd-bulk" "worker-1" 2>/dev/null
release_bead_locks "nd-bulk" 2>/dev/null
remaining=$(find "$TEST_LOCK_DIR" -maxdepth 1 -name "nd-bulk-*" -type f 2>/dev/null | wc -l)
if [[ $remaining -eq 0 ]]; then
    test_pass
else
    test_fail "Expected 0 locks remaining, found $remaining"
fi

# Test 13: release_bead_locks only removes that bead's locks
test_case "release_bead_locks preserves other beads' locks"
reset_locks
checkout_file "/tmp/test-preserve1.sh" "nd-keeper" "worker-1" 2>/dev/null
checkout_file "/tmp/test-preserve2.sh" "nd-releaser" "worker-2" 2>/dev/null
release_bead_locks "nd-releaser" 2>/dev/null
keeper_locks=$(find "$TEST_LOCK_DIR" -maxdepth 1 -name "nd-keeper-*" -type f 2>/dev/null | wc -l)
if [[ $keeper_locks -eq 1 ]]; then
    test_pass
else
    test_fail "Expected 1 lock for nd-keeper, found $keeper_locks"
fi

# Test 14: check_file returns 0 (locked) when file is locked
test_case "check_file returns 0 when file is locked"
reset_locks
checkout_file "/tmp/test-checkfile.sh" "nd-check1" "worker-1" 2>/dev/null
if check_file "/tmp/test-checkfile.sh" >/dev/null 2>/dev/null; then
    test_pass
else
    test_fail "Expected check_file to return 0 (locked)"
fi

# Test 15: check_file returns 1 when file is not locked
test_case "check_file returns 1 when file is not locked"
reset_locks
if check_file "/tmp/test-notlocked.sh" >/dev/null 2>/dev/null; then
    test_fail "Expected check_file to return 1 (free)"
else
    test_pass
fi

# Test 16: check_file prints lock info JSON
test_case "check_file prints lock info JSON with correct bead"
reset_locks
checkout_file "/tmp/test-checkjson.sh" "nd-jsoncheck" "worker-json" 2>/dev/null
info=$(check_file "/tmp/test-checkjson.sh" 2>/dev/null)
bead=$(echo "$info" | jq -r '.bead' 2>/dev/null)
if [[ "$bead" == "nd-jsoncheck" ]]; then
    test_pass
else
    test_fail "Expected bead 'nd-jsoncheck', got '$bead'"
fi

# Test 17: list_locks returns empty array when no locks
test_case "list_locks returns empty array when no locks"
reset_locks
output=$(list_locks 2>/dev/null)
if [[ "$output" == "[]" ]]; then
    test_pass
else
    test_fail "Expected '[]', got '$output'"
fi

# Test 18: list_locks returns all active locks
test_case "list_locks returns all active locks"
reset_locks
checkout_file "/tmp/test-list1.sh" "nd-list" "worker-1" 2>/dev/null
checkout_file "/tmp/test-list2.sh" "nd-list" "worker-1" 2>/dev/null
output=$(list_locks 2>/dev/null)
count=$(echo "$output" | jq 'length' 2>/dev/null)
if [[ "$count" -eq 2 ]]; then
    test_pass
else
    test_fail "Expected 2 locks, got count=$count"
fi

# Test 19: list_locks filters by bead_id
test_case "list_locks filters by bead_id"
reset_locks
checkout_file "/tmp/test-filter1.sh" "nd-aaa" "worker-1" 2>/dev/null
checkout_file "/tmp/test-filter2.sh" "nd-bbb" "worker-2" 2>/dev/null
output=$(list_locks "nd-aaa" 2>/dev/null)
count=$(echo "$output" | jq 'length' 2>/dev/null)
if [[ "$count" -eq 1 ]]; then
    test_pass
else
    test_fail "Expected 1 lock for nd-aaa, got count=$count"
fi

# Test 20: checkout_file requires bead_id
test_case "checkout_file fails without bead_id"
reset_locks
checkout_file "/tmp/test-nobead.sh" "" "" 2>/dev/null && nobead_status=0 || nobead_status=$?
if [[ $nobead_status -ne 0 ]]; then
    test_pass
else
    test_fail "Expected failure without bead_id"
fi

# Test 21: check_stale_locks detects stale locks
test_case "check_stale_locks detects old locks"
reset_locks
checkout_file "/tmp/test-stale.sh" "nd-stale" "worker-1" 2>/dev/null
# Backdate the lock's timestamp by overwriting with old ts
uuid=$(_needle_lock_path_uuid "/tmp/test-stale.sh")
lock_file="$TEST_LOCK_DIR/nd-stale-$uuid"
jq --argjson ts "$(($(date +%s) - 7200))" '.ts = $ts' "$lock_file" > "${lock_file}.tmp" && mv "${lock_file}.tmp" "$lock_file"
# check_stale_locks returns number of stale locks as exit code (non-zero = found stale)
stale_count=0
check_stale_locks "warn" 2>/dev/null || stale_count=$?
if [[ "$stale_count" -ge 1 ]]; then
    test_pass
else
    test_fail "Expected at least 1 stale lock, got stale_count=$stale_count"
fi

# Test 22: check_stale_locks with release action removes stale locks
test_case "check_stale_locks with release action removes lock"
reset_locks
checkout_file "/tmp/test-stale2.sh" "nd-stale2" "worker-1" 2>/dev/null
uuid=$(_needle_lock_path_uuid "/tmp/test-stale2.sh")
lock_file="$TEST_LOCK_DIR/nd-stale2-$uuid"
jq --argjson ts "$(($(date +%s) - 7200))" '.ts = $ts' "$lock_file" > "${lock_file}.tmp" && mv "${lock_file}.tmp" "$lock_file"
check_stale_locks "release" 2>/dev/null || true
if [[ ! -f "$lock_file" ]]; then
    test_pass
else
    test_fail "Expected stale lock to be released"
fi

# Test 23: _needle_lock_extract_bead_id extracts correct bead id
test_case "_needle_lock_extract_bead_id extracts bead ID from filename"
lock_name="nd-2ov-a7f3c821"
bead=$(_needle_lock_extract_bead_id "$TEST_LOCK_DIR/$lock_name")
if [[ "$bead" == "nd-2ov" ]]; then
    test_pass
else
    test_fail "Expected 'nd-2ov', got '$bead'"
fi

# Test 24: _needle_lock_extract_bead_id handles compound bead ids
test_case "_needle_lock_extract_bead_id handles multi-segment bead IDs"
lock_name="bd-muv-a7f3c821"
bead=$(_needle_lock_extract_bead_id "$TEST_LOCK_DIR/$lock_name")
if [[ "$bead" == "bd-muv" ]]; then
    test_pass
else
    test_fail "Expected 'bd-muv', got '$bead'"
fi

# Test 25: release_file is idempotent (no error if lock doesn't exist)
test_case "release_file is idempotent (no error if not locked)"
reset_locks
if release_file "/tmp/nonexistent.sh" "nd-noop" 2>/dev/null; then
    test_pass
else
    test_fail "Expected release_file to succeed even if no lock exists"
fi

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
