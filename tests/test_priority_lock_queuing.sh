#!/usr/bin/env bash
# Tests for NEEDLE priority-based lock queuing with bump signals
#
# This test suite verifies:
# - Lock files include queue structure (holder + queue array)
# - request_lock_with_priority() acquires or queues based on priority
# - _needle_signal_worker() creates signal files
# - _needle_check_priority_bumps() detects and returns signals
# - handle_priority_bump() processes priority bump signals
# - _needle_release_lock_with_handoff() notifies next in queue

set -euo pipefail

# Test setup
TEST_DIR=$(mktemp -d)
TEST_LOCK_DIR="$TEST_DIR/needle-locks"
TEST_QUEUE_DIR="$TEST_DIR/needle-locks/queue"

# Source the module
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_LOCK_DIR="$TEST_LOCK_DIR"
export NEEDLE_QUEUE_DIR="$TEST_QUEUE_DIR"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false
export NEEDLE_LOG_INITIALIZED=true
export NEEDLE_SESSION="test-session-priority"

# Stub telemetry to avoid side effects
_needle_telemetry_emit() { return 0; }
_needle_metrics_record_event() { return 0; }

# Source required modules
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lock/checkout.sh"

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
    mkdir -p "$TEST_QUEUE_DIR"
}

# ============================================================================
# Tests
# ============================================================================

echo "=== Priority-Based Lock Queuing Tests ==="
echo ""

# ============================================================================
# Test 1: Queue Structure in Lock Files
# ============================================================================

echo "--- Queue Structure ---"

test_case "_needle_lock_write_with_queue creates correct structure"
reset_locks
lock_file="$TEST_LOCK_DIR/nd-test-$(echo -n '/test/file.sh' | md5sum | cut -c1-8)"
_needle_lock_write_with_queue "$lock_file" "nd-test" "worker-1" "/test/file.sh" "/workspace" "0"

# Verify the lock file has holder and queue structure
if command -v jq &>/dev/null; then
    holder=$(jq -r '.holder.bead' "$lock_file")
    priority=$(jq -r '.holder.priority' "$lock_file")
    queue_len=$(jq -r '.queue | length' "$lock_file")

    if [[ "$holder" == "nd-test" ]] && [[ "$priority" == "0" ]] && [[ "$queue_len" == "0" ]]; then
        test_pass
    else
        test_fail "holder=$holder priority=$priority queue_len=$queue_len"
    fi
else
    test_fail "jq required for queue structure tests"
fi

test_case "_needle_lock_write_with_queue stores correct priority"
reset_locks
lock_file="$TEST_LOCK_DIR/nd-p1-$(echo -n '/test/file2.sh' | md5sum | cut -c1-8)"
_needle_lock_write_with_queue "$lock_file" "nd-p1" "worker-2" "/test/file2.sh" "/workspace" "1"

if command -v jq &>/dev/null; then
    priority=$(jq -r '.holder.priority' "$lock_file")
    if [[ "$priority" == "1" ]]; then
        test_pass
    else
        test_fail "Expected priority 1, got $priority"
    fi
else
    test_fail "jq required for priority tests"
fi

# ============================================================================
# Test 2: request_lock_with_priority() - Acquire Free File
# ============================================================================

echo ""
echo "--- request_lock_with_priority: Acquire Free File ---"

test_case "request_lock_with_priority acquires free file immediately"
reset_locks
if request_lock_with_priority "/tmp/free-file.sh" "nd-acquirer" "0" "worker-1" 2>/dev/null; then
    # Check lock was created
    path_uuid=$(echo -n "/tmp/free-file.sh" | md5sum | cut -c1-8)
    lock_file="$TEST_LOCK_DIR/nd-acquirer-$path_uuid"
    if [[ -f "$lock_file" ]]; then
        test_pass
    else
        test_fail "Lock file not created"
    fi
else
    test_fail "Failed to acquire free file"
fi

test_case "request_lock_with_priority stores priority in lock"
reset_locks
request_lock_with_priority "/tmp/check-priority.sh" "nd-p2" "2" "worker-2" 2>/dev/null
path_uuid=$(echo -n "/tmp/check-priority.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-p2-$path_uuid"

if command -v jq &>/dev/null && [[ -f "$lock_file" ]]; then
    stored_priority=$(jq -r '.holder.priority' "$lock_file")
    if [[ "$stored_priority" == "2" ]]; then
        test_pass
    else
        test_fail "Expected priority 2, got $stored_priority"
    fi
else
    test_fail "Lock file not created or jq unavailable"
fi

# ============================================================================
# Test 3: _needle_lock_add_to_queue() - Add to Queue
# ============================================================================

echo ""
echo "--- _needle_lock_add_to_queue: Queue Management ---"

test_case "_needle_lock_add_to_queue adds waiting bead to queue"
reset_locks
# First, create a lock file manually with queue structure
path_uuid=$(echo -n "/tmp/queued-file.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-holder-$path_uuid"
cat > "$lock_file" << EOF
{
  "path": "/tmp/queued-file.sh",
  "holder": {"bead": "nd-holder", "priority": 2, "worker": "worker-holder"},
  "queue": []
}
EOF

# Add a high-priority request to the queue
if _needle_lock_add_to_queue "/tmp/queued-file.sh" "nd-high" "worker-high" "0" 2>/dev/null; then
    # Verify queue has one entry
    if command -v jq &>/dev/null; then
        queue_len=$(jq -r '.queue | length' "$lock_file")
        queued_bead=$(jq -r '.queue[0].bead' "$lock_file")
        queued_priority=$(jq -r '.queue[0].priority' "$lock_file")

        if [[ "$queue_len" == "1" ]] && [[ "$queued_bead" == "nd-high" ]] && [[ "$queued_priority" == "0" ]]; then
            test_pass
        else
            test_fail "queue_len=$queue_len bead=$queued_bead priority=$queued_priority"
        fi
    else
        test_fail "jq required for queue verification"
    fi
else
    test_fail "Failed to add to queue"
fi

test_case "_needle_lock_add_to_queue preserves timestamp"
reset_locks
path_uuid=$(echo -n "/tmp/timestamp-file.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-holder2-$path_uuid"
cat > "$lock_file" << EOF
{
  "path": "/tmp/timestamp-file.sh",
  "holder": {"bead": "nd-holder2", "priority": 2, "worker": "worker-holder2"},
  "queue": []
}
EOF

before_ts=$(date +%s)
_needle_lock_add_to_queue "/tmp/timestamp-file.sh" "nd-ts" "worker-ts" "1" 2>/dev/null
after_ts=$(date +%s)

if command -v jq &>/dev/null; then
    queued_ts=$(jq -r '.queue[0].ts' "$lock_file")
    if [[ "$queued_ts" -ge "$before_ts" ]] && [[ "$queued_ts" -le "$after_ts" ]]; then
        test_pass
    else
        test_fail "Timestamp out of range: $queued_ts (expected $before_ts-$after_ts)"
    fi
else
    test_fail "jq required for timestamp verification"
fi

# ============================================================================
# Test 4: _needle_signal_worker() - Signal Creation
# ============================================================================

echo ""
echo "--- _needle_signal_worker: Signal Files ---"

test_case "_needle_signal_worker creates signal file"
reset_locks
if _needle_signal_worker "nd-holder" "worker-holder" "PRIORITY_BUMP" "/test/file.sh" "nd-waiter" "0" 2>/dev/null; then
    queue_file="$TEST_QUEUE_DIR/nd-holder.queue"
    if [[ -f "$queue_file" ]] && [[ -s "$queue_file" ]]; then
        test_pass
    else
        test_fail "Queue file not created or empty"
    fi
else
    test_fail "Failed to send signal"
fi

test_case "_needle_signal_worker signal contains correct data"
reset_locks
_needle_signal_worker "nd-signal-test" "worker-test" "PRIORITY_BUMP" "/signal/test.sh" "nd-p0" "0" 2>/dev/null
queue_file="$TEST_QUEUE_DIR/nd-signal-test.queue"

if command -v jq &>/dev/null; then
    signal=$(cat "$queue_file")
    signal_type=$(echo "$signal" | jq -r '.type')
    waiting_bead=$(echo "$signal" | jq -r '.waiting_bead')
    waiting_priority=$(echo "$signal" | jq -r '.waiting_priority')
    filepath=$(echo "$signal" | jq -r '.path')

    if [[ "$signal_type" == "PRIORITY_BUMP" ]] && \
       [[ "$waiting_bead" == "nd-p0" ]] && \
       [[ "$waiting_priority" == "0" ]] && \
       [[ "$filepath" == "/signal/test.sh" ]]; then
        test_pass
    else
        test_fail "signal_type=$signal_type bead=$waiting_bead priority=$waiting_priority path=$filepath"
    fi
else
    test_fail "jq required for signal verification"
fi

test_case "_needle_signal_worker appends multiple signals"
reset_locks
_needle_signal_worker "nd-multi" "worker-1" "PRIORITY_BUMP" "/file1.sh" "nd-w1" "0" 2>/dev/null
_needle_signal_worker "nd-multi" "worker-2" "PRIORITY_BUMP" "/file2.sh" "nd-w2" "1" 2>/dev/null
queue_file="$TEST_QUEUE_DIR/nd-multi.queue"

if command -v jq &>/dev/null; then
    # Use jq to count the number of JSON objects (one per line)
    signal_count=$(jq -s 'length' "$queue_file" 2>/dev/null)
    if [[ "$signal_count" == "2" ]]; then
        test_pass
    else
        test_fail "Expected 2 signals, got $signal_count"
    fi
else
    # Fallback: just check file has multiple lines
    signal_count=$(wc -l < "$queue_file" | tr -d ' ')
    if [[ "$signal_count" == "2" ]]; then
        test_pass
    else
        test_fail "Expected 2 signals (line count), got $signal_count"
    fi
fi

# ============================================================================
# Test 5: _needle_check_priority_bumps() - Signal Detection
# ============================================================================

echo ""
echo "--- _needle_check_priority_bumps: Signal Detection ---"

test_case "_needle_check_priority_bumps returns empty array when no signals"
reset_locks
export NEEDLE_BEAD_ID="nd-empty-test"
result=$(_needle_check_priority_bumps "nd-empty-test" 2>/dev/null)

if command -v jq &>/dev/null; then
    result_len=$(echo "$result" | jq 'length')
    if [[ "$result_len" == "0" ]]; then
        test_pass
    else
        test_fail "Expected empty array, got length $result_len"
    fi
else
    if [[ "$result" == "[]" ]]; then
        test_pass
    else
        test_fail "Expected '[]', got '$result'"
    fi
fi

test_case "_needle_check_priority_bumps detects and returns signals"
reset_locks
# Create signal file
cat > "$TEST_QUEUE_DIR/nd-check-test.queue" << EOF
{"type":"PRIORITY_BUMP","path":"/test/priority.sh","waiting_bead":"nd-high","waiting_priority":"0","ts":1709337700}
EOF

export NEEDLE_BEAD_ID="nd-check-test"
# Note: _needle_check_priority_bumps returns the count as exit code
# Use || true to prevent script exit on non-zero return with set -euo pipefail
result=$(_needle_check_priority_bumps "nd-check-test" 2>/dev/null || true)

if command -v jq &>/dev/null; then
    result_len=$(echo "$result" | jq 'length')
    signal_type=$(echo "$result" | jq -r '.[0].type')
    waiting_bead=$(echo "$result" | jq -r '.[0].waiting_bead')

    if [[ "$result_len" == "1" ]] && [[ "$signal_type" == "PRIORITY_BUMP" ]] && [[ "$waiting_bead" == "nd-high" ]]; then
        test_pass
    else
        test_fail "result_len=$result_len type=$signal_type bead=$waiting_bead"
    fi
else
    # Check result is not empty array
    if [[ "$result" != "[]" ]]; then
        test_pass
    else
        test_fail "Expected non-empty result"
    fi
fi

test_case "_needle_check_priority_bumps clears queue file after reading"
reset_locks
cat > "$TEST_QUEUE_DIR/nd-clear-test.queue" << EOF
{"type":"PRIORITY_BUMP","path":"/test/clear.sh","waiting_bead":"nd-clear","waiting_priority":"0","ts":1709337700}
EOF

export NEEDLE_BEAD_ID="nd-clear-test"
_needle_check_priority_bumps "nd-clear-test" 2>/dev/null || true

queue_file="$TEST_QUEUE_DIR/nd-clear-test.queue"
if [[ ! -f "$queue_file" ]] || [[ ! -s "$queue_file" ]]; then
    test_pass
else
    test_fail "Queue file should be cleared after reading"
fi

# ============================================================================
# Test 6: handle_priority_bump() - Signal Handling
# ============================================================================

echo ""
echo "--- handle_priority_bump: Signal Handling ---"

test_case "handle_priority_bump processes valid signal JSON"
reset_locks
export NEEDLE_BEAD_ID="nd-handler-test"

# Create a signal JSON
signal_json='{"type":"PRIORITY_BUMP","path":"/test/handle.sh","waiting_bead":"nd-waiter","waiting_priority":"0","ts":1709337700}'

# This should not error
if handle_priority_bump "$signal_json" 2>/dev/null; then
    test_pass
else
    test_fail "handle_priority_bump should not error on valid signal"
fi

# ============================================================================
# Test 7: _needle_release_lock_with_handoff() - Queue Handoff
# ============================================================================

echo ""
echo "--- _needle_release_lock_with_handoff: Queue Handoff ---"

test_case "_needle_release_lock_with_handoff removes lock file"
reset_locks
path_uuid=$(echo -n "/tmp/handoff-file.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-handoff-$path_uuid"

# Create a lock file with empty queue
cat > "$lock_file" << EOF
{
  "path": "/tmp/handoff-file.sh",
  "holder": {"bead": "nd-handoff", "priority": 2, "worker": "worker-handoff"},
  "queue": []
}
EOF

_needle_release_lock_with_handoff "/tmp/handoff-file.sh" "nd-handoff" 2>/dev/null

if [[ ! -f "$lock_file" ]]; then
    test_pass
else
    test_fail "Lock file should be removed after release"
fi

test_case "_needle_release_lock_with_handoff signals next in queue"
reset_locks
path_uuid=$(echo -n "/tmp/next-queue.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-current-$path_uuid"

# Create a lock file with queued waiters
cat > "$lock_file" << EOF
{
  "path": "/tmp/next-queue.sh",
  "holder": {"bead": "nd-current", "priority": 2, "worker": "worker-current"},
  "queue": [
    {"bead": "nd-next", "priority": 0, "worker": "worker-next", "ts": 1709337700}
  ]
}
EOF

_needle_release_lock_with_handoff "/tmp/next-queue.sh" "nd-current" 2>/dev/null

# Check that nd-next was signaled
next_queue_file="$TEST_QUEUE_DIR/nd-next.queue"
if [[ -f "$next_queue_file" ]]; then
    if command -v jq &>/dev/null; then
        signal=$(cat "$next_queue_file")
        signal_type=$(echo "$signal" | jq -r '.type')
        if [[ "$signal_type" == "LOCK_AVAILABLE" ]]; then
            test_pass
        else
            test_fail "Expected LOCK_AVAILABLE signal, got $signal_type"
        fi
    else
        test_pass "Signal file created (jq not available for verification)"
    fi
else
    test_fail "Next in queue should be signaled"
fi

# ============================================================================
# Test 8: Priority Comparison - Higher Priority Queues
# ============================================================================

echo ""
echo "--- Priority Comparison: Queue vs Immediate ---"

test_case "Lower priority returns 1 (dependency added)"
reset_locks
# Create a lock held by P0
path_uuid=$(echo -n "/tmp/p0-file.sh" | md5sum | cut -c1-8)
lock_file="$TEST_LOCK_DIR/nd-p0-$path_uuid"
cat > "$lock_file" << EOF
{
  "path": "/tmp/p0-file.sh",
  "holder": {"bead": "nd-p0", "priority": 0, "worker": "worker-p0"},
  "queue": []
}
EOF

# P1 tries to acquire - should return 1 (dependency)
# Use if-then-else to handle set -e exit on non-zero return
if request_lock_with_priority "/tmp/p0-file.sh" "nd-p1" "1" "worker-p1" 2>/dev/null; then
    result=0
else
    result=$?
fi

if [[ "$result" == "1" ]]; then
    test_pass
else
    test_fail "Expected return code 1, got $result"
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
