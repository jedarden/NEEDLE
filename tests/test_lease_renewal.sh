#!/usr/bin/env bash
# Test suite for NEEDLE lock lease renewal with heartbeat integration
#
# Tests:
#   - Lease renewal for active locks
#   - Lease expiration for stale locks
#   - Grace period handling
#   - Worker alive detection via heartbeat
#   - Config option handling (duration, renewal_interval, grace_period)
#   - get_expiring_locks query function

set -euo pipefail

# ============================================================================
# Test Setup
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_SRC="${NEEDLE_SRC:-$SCRIPT_DIR/../src}"
NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"

# Source the lease module
source "$NEEDLE_SRC/lock/lease.sh"

# Source telemetry for event testing
source "$NEEDLE_SRC/telemetry/events.sh"

# Test directory (use a temp directory that we clean up)
TEST_DIR=""
TEST_LOCK_DIR=""
TEST_HEARTBEAT_DIR=""

# Mock config values
setup() {
    TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/needle-lease-test-XXXXXXXX")
    TEST_LOCK_DIR="${TEST_DIR}/locks"
    TEST_HEARTBEAT_DIR="${TEST_DIR}/heartbeats"

    # Override lock directory for tests
    NEEDLE_LOCK_DIR="$TEST_LOCK_DIR"
    NEEDLE_STATE_DIR="${TEST_DIR}/state"

    mkdir -p "$TEST_LOCK_DIR"
    mkdir -p "$TEST_HEARTBEAT_DIR"
    mkdir -p "$NEEDLE_STATE_DIR/heartbeats"

    # Mock _needle_config_get to return test values
    _needle_config_get() {
        local key="$1"
        local default="${2:-}"

        case "$key" in
            "file_locks.lease.duration") echo "60s" ;;
            "file_locks.lease.renewal_interval") echo "15s" ;;
            "file_locks.lease.grace_period") echo "10s" ;;
            "watchdog.heartbeat_timeout") echo "120" ;;
            *) echo "$default" ;;
        esac
    }

    echo "=== Lock Lease Renewal Tests ===" >&2
}

teardown() {
    if [[ -n "$TEST_DIR" ]] && [[ -d "$TEST_DIR" ]]; then
        rm -rf "$TEST_DIR"
    fi
}

# ============================================================================
# Helper Functions
# ============================================================================

# Create a test lock file
create_test_lock() {
    local bead_id="$1"
    local filepath="$2"
    local worker_id="${3:-test-worker}"
    local timestamp="${4:-$(date +%s)}"

    local path_uuid
    path_uuid=$(echo -n "$filepath" | md5sum | cut -c1-8)
    local lock_file="${TEST_LOCK_DIR}/${bead_id}-${path_uuid}"

    local workspace="/tmp/test-workspace"

    # Create lock file JSON
    jq -n \
        --arg bead "$bead_id" \
        --arg worker "$worker_id" \
        --arg path "$filepath" \
        --arg type "write" \
        --argjson ts "$timestamp" \
        --arg workspace "$workspace" \
        '{bead: $bead, worker: $worker, path: $path, type: $type, ts: $ts, workspace: $workspace}' \
        > "$lock_file"

    echo "$lock_file"
}

# Create a test heartbeat file
create_test_heartbeat() {
    local worker_id="$1"
    local status="${2:-idle}"
    local current_bead="${3:-}"
    local timestamp="${4:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"

    local heartbeat_file="${NEEDLE_STATE_DIR}/heartbeats/${worker_id}.json"

    jq -n \
        --arg worker "$worker_id" \
        --arg pid "$$" \
        --arg started "$timestamp" \
        --arg last_heartbeat "$timestamp" \
        --arg status "$status" \
        --arg current_bead "$current_bead" \
        --arg strand "" \
        --arg workspace "/tmp/test" \
        --arg agent "test" \
        --argjson queue_depth 0 \
        '{worker: $worker, pid: ($pid | tonumber), started: $started, last_heartbeat: $last_heartbeat, status: $status, current_bead: (if $current_bead == "" then null else $current_bead end), strand: (if $strand == "" then null else $strand end), workspace: $workspace, agent: $agent, queue_depth: $queue_depth}' \
        > "$heartbeat_file"

    echo "$heartbeat_file"
}

# Get lock timestamp
get_lock_ts() {
    local lock_file="$1"
    jq -r '.ts' "$lock_file"
}

# ============================================================================
# Test: Config Helpers
# ============================================================================
test_config_helpers() {
    echo "Testing config helpers..."

    local duration interval grace

    duration=$(_needle_lease_get_duration)
    if [[ "$duration" != "60" ]]; then
        echo "FAIL: Expected duration 60, got $duration"
        return 1
    fi

    interval=$(_needle_lease_get_renewal_interval)
    if [[ "$interval" != "15" ]]; then
        echo "FAIL: Expected interval 15, got $interval"
        return 1
    fi

    grace=$(_needle_lease_get_grace_period)
    if [[ "$grace" != "10" ]]; then
        echo "FAIL: Expected grace 10, got $grace"
        return 1
    fi

    echo "PASS: Config helpers"
    return 0
}

# ============================================================================
# Test: Lock Lease Renewal
# ============================================================================
test_renew_lock_leases() {
    echo "Testing lock lease renewal..."

    local bead_id="nd-renew-test"
    local test_file="/tmp/test-renew-file.txt"

    # Create a test lock with old timestamp
    local old_ts
    old_ts=$(date -d '2 minutes ago' +%s 2>/dev/null || echo "$(($(date +%s) - 120))")

    create_test_lock "$bead_id" "$test_file" "test-worker" "$old_ts"

    # Set NEEDLE_BEAD_ID for the function
    export NEEDLE_BEAD_ID="$bead_id"

    # Renew locks
    local renewed
    renewed=$(renew_lock_leases "$bead_id")

    if [[ "$renewed" != "1" ]]; then
        echo "FAIL: Expected 1 lock renewed, got $renewed"
        return 1
    fi

    # Verify timestamp was updated
    local path_uuid lock_file new_ts
    path_uuid=$(echo -n "$test_file" | md5sum | cut -c1-8)
    lock_file="${TEST_LOCK_DIR}/${bead_id}-${path_uuid}"
    new_ts=$(get_lock_ts "$lock_file")

    if [[ "$new_ts" -le "$old_ts" ]]; then
        echo "FAIL: Lock timestamp was not updated (old: $old_ts, new: $new_ts)"
        return 1
    fi

    unset NEEDLE_BEAD_ID

    echo "PASS: Lock lease renewal"
    return 0
}

# ============================================================================
# Test: Renew Multiple Locks
# ============================================================================
test_renew_multiple_locks() {
    echo "Testing multiple lock renewal..."

    local bead_id="nd-multi-renew"

    # Create multiple locks for the same bead
    create_test_lock "$bead_id" "/tmp/file1.txt"
    create_test_lock "$bead_id" "/tmp/file2.txt"
    create_test_lock "$bead_id" "/tmp/file3.txt"

    # Renew locks
    local renewed
    renewed=$(renew_lock_leases "$bead_id")

    if [[ "$renewed" != "3" ]]; then
        echo "FAIL: Expected 3 locks renewed, got $renewed"
        return 1
    fi

    echo "PASS: Multiple lock renewal"
    return 0
}

# ============================================================================
# Test: Renew No Bead ID
# ============================================================================
test_renew_no_bead_id() {
    echo "Testing renewal without bead ID..."

    unset NEEDLE_BEAD_ID

    local renewed
    renewed=$(renew_lock_leases "" 2>/dev/null || echo "0")

    if [[ "$renewed" != "0" ]]; then
        echo "FAIL: Expected 0 locks renewed (no bead ID), got $renewed"
        return 1
    fi

    echo "PASS: Renewal without bead ID returns 0"
    return 0
}

# ============================================================================
# Test: Lease Expiration - Stale Lock
# ============================================================================
test_expire_stale_lock() {
    echo "Testing stale lock expiration..."

    local bead_id="nd-stale-test"
    local worker_id="dead-worker"

    # Create a heartbeat file that's old (beyond timeout)
    local old_heartbeat_ts
    old_heartbeat_ts=$(date -d '3 minutes ago' -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-3M +%Y-%m-%dT%H:%M:%SZ)

    create_test_heartbeat "$worker_id" "executing" "$bead_id" "$old_heartbeat_ts"

    # Create a lock that's old (beyond lease + grace)
    local old_ts
    old_ts=$(date -d '2 minutes ago' +%s 2>/dev/null || echo "$(($(date +%s) - 120))")

    create_test_lock "$bead_id" "/tmp/stale-file.txt" "$worker_id" "$old_ts"

    # Expire stale locks
    local expired
    expired=$(expire_stale_leases)

    if [[ "$expired" != "1" ]]; then
        echo "FAIL: Expected 1 lock expired, got $expired"
        return 1
    fi

    echo "PASS: Stale lock expiration"
    return 0
}

# ============================================================================
# Test: Lease Expiration - Alive Worker Not Expired
# ============================================================================
test_alive_worker_not_expired() {
    echo "Testing that alive worker's locks are not expired..."

    local bead_id="nd-alive-test"
    local worker_id="alive-worker"

    # Create a fresh heartbeat file
    create_test_heartbeat "$worker_id" "executing" "$bead_id"

    # Create a lock that's old but worker has heartbeat
    local old_ts
    old_ts=$(date -d '2 minutes ago' +%s 2>/dev/null || echo "$(($(date +%s) - 120))")

    create_test_lock "$bead_id" "/tmp/alive-file.txt" "$worker_id" "$old_ts"

    # Expire stale locks
    local expired
    expired=$(expire_stale_leases)

    if [[ "$expired" != "0" ]]; then
        echo "FAIL: Expected 0 locks expired (worker alive), got $expired"
        return 1
    fi

    echo "PASS: Alive worker's locks not expired"
    return 0
}

# ============================================================================
# Test: Worker Alive Detection
# ============================================================================
test_worker_alive_detection() {
    echo "Testing worker alive detection..."

    local worker_id="heartbeat-test-worker"
    local bead_id="nd-heartbeat-test"

    # Test with fresh heartbeat
    create_test_heartbeat "$worker_id" "executing" "$bead_id"

    if ! _needle_lease_worker_alive "$worker_id" "$bead_id"; then
        echo "FAIL: Worker with fresh heartbeat should be alive"
        return 1
    fi

    # Test with stale heartbeat
    local old_ts
    old_ts=$(date -d '3 minutes ago' -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-3M +%Y-%m-%dT%H:%M:%SZ)

    create_test_heartbeat "$worker_id" "idle" "" "$old_ts"

    if _needle_lease_worker_alive "$worker_id" "$bead_id"; then
        echo "FAIL: Worker with stale heartbeat should be dead"
        return 1
    fi

    echo "PASS: Worker alive detection"
    return 0
}

# ============================================================================
# Test: Get Expiring Locks
# ============================================================================
test_get_expiring_locks() {
    echo "Testing get_expiring_locks..."

    local bead_id="nd-expiring-test"

    # Create locks with different ages
    local now
    now=$(date +%s)

    # Lock expiring soon (50 seconds old)
    create_test_lock "$bead_id" "/tmp/expiring-soon.txt" "worker1" "$((now - 50))"

    # Lock not expiring soon (10 seconds old)
    create_test_lock "$bead_id" "/tmp/fresh-lock.txt" "worker2" "$((now - 10))"

    # Get locks expiring within 30 seconds
    local expiring
    expiring=$(get_expiring_locks 30)

    if ! echo "$expiring" | grep -q "expiring-soon.txt"; then
        echo "FAIL: Should find expiring lock"
        return 1
    fi

    if echo "$expiring" | grep -q "fresh-lock.txt"; then
        echo "FAIL: Should not include fresh lock"
        return 1
    fi

    echo "PASS: Get expiring locks"
    return 0
}

# ============================================================================
# Test: Config with Different Values
# ============================================================================
test_custom_config_values() {
    echo "Testing custom config values..."

    # Mock different config values
    _needle_config_get() {
        local key="$1"
        local default="${2:-}"

        case "$key" in
            "file_locks.lease.duration") echo "120s" ;;
            "file_locks.lease.renewal_interval") echo "30s" ;;
            "file_locks.lease.grace_period") echo "20s" ;;
            "watchdog.heartbeat_timeout") echo "60" ;;
            *) echo "$default" ;;
        esac
    }

    local duration interval grace

    duration=$(_needle_lease_get_duration)
    if [[ "$duration" != "120" ]]; then
        echo "FAIL: Expected custom duration 120, got $duration"
        return 1
    fi

    interval=$(_needle_lease_get_renewal_interval)
    if [[ "$interval" != "30" ]]; then
        echo "FAIL: Expected custom interval 30, got $interval"
        return 1
    fi

    grace=$(_needle_lease_get_grace_period)
    if [[ "$grace" != "20" ]]; then
        echo "FAIL: Expected custom grace 20, got $grace"
        return 1
    fi

    # Restore original mock
    _needle_config_get() {
        local key="$1"
        local default="${2:-}"

        case "$key" in
            "file_locks.lease.duration") echo "60s" ;;
            "file_locks.lease.renewal_interval") echo "15s" ;;
            "file_locks.lease.grace_period") echo "10s" ;;
            "watchdog.heartbeat_timeout") echo "120" ;;
            *) echo "$default" ;;
        esac
    }

    echo "PASS: Custom config values"
    return 0
}

# ============================================================================
# Run All Tests
# ============================================================================
run_tests() {
    local failed=0
    local passed=0

    setup

    # List of all test functions
    local tests=(
        "test_config_helpers"
        "test_renew_lock_leases"
        "test_renew_multiple_locks"
        "test_renew_no_bead_id"
        "test_expire_stale_lock"
        "test_alive_worker_not_expired"
        "test_worker_alive_detection"
        "test_get_expiring_locks"
        "test_custom_config_values"
    )

    # Run each test
    for test_func in "${tests[@]}"; do
        echo ""
        echo "Running: $test_func"

        # Clean up between tests
        rm -f "${TEST_LOCK_DIR}"/* 2>/dev/null || true
        rm -f "${NEEDLE_STATE_DIR}/heartbeats"/* 2>/dev/null || true

        # Run test in subshell to isolate state
        if ( $test_func ); then
            passed=$((passed + 1))
        else
            failed=$((failed + 1))
        fi
    done

    echo ""
    echo "===================================="
    echo "Tests passed: $passed"
    echo "Tests failed: $failed"
    echo "===================================="

    teardown

    if [[ $failed -gt 0 ]]; then
        return 1
    fi

    return 0
}

# Run tests if executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    run_tests
    exit $?
fi
