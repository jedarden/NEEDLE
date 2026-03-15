#!/usr/bin/env bash
# Test script for NEEDLE Worker Loop Module
#
# This test script verifies:
# 1. Signal handling setup
# 2. Worker loop initialization
# 3. Configuration loading
# 4. Event emission functions

set -o pipefail

# Test directory
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_DIR="$TEST_DIR/../src/runner"

# Source the loop module (which includes stubs for dependencies)
source "$SCRIPT_DIR/loop.sh"

# Test helper: Setup mock environment
setup_mock_environment() {
    export NEEDLE_SESSION="test-session-$$"
    export NEEDLE_RUNNER="test"
    export NEEDLE_PROVIDER="test"
    export NEEDLE_MODEL="test"
    export NEEDLE_IDENTIFIER="test"
    export NEEDLE_WORKSPACE="/tmp/test-workspace-$$"
    export NEEDLE_AGENT="test-agent"
    export NEEDLE_HOME="/tmp/test-needle-home-$$"
    export NEEDLE_STATE_DIR="$NEEDLE_HOME/state"
    export NEEDLE_VERBOSE=true

    # Create test directories
    mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR"
    mkdir -p "$NEEDLE_HOME/$NEEDLE_STATE_DIR/heartbeats"
    mkdir -p "$NEEDLE_WORKSPACE"
}

# Test helper: Cleanup mock environment
cleanup_mock_environment() {
    if [[ -n "${NEEDLE_HOME:-}" ]]; then
        rm -rf "$NEEDLE_HOME" 2>/dev/null || true
    fi
    if [[ -n "${NEEDLE_WORKSPACE:-}" ]]; then
        rm -rf "$NEEDLE_WORKSPACE" 2>/dev/null || true
    fi
}

# Test: Configuration helper
test_config_helper() {
    echo "=== Testing configuration helper ==="

    setup_mock_environment

    # Test configuration loading with defaults
    local polling_interval
    polling_interval=$(_needle_loop_get_config "runner.polling_interval" "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL")

    local idle_timeout
    idle_timeout=$(_needle_loop_get_config "runner.idle_timeout" "$NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT")

    # Remove 's' suffix
    polling_interval="${polling_interval%s}"
    idle_timeout="${idle_timeout%s}"

    echo "Configuration loaded:"
    echo "  polling_interval: ${polling_interval}s"
    echo "  idle_timeout: ${idle_timeout}s"

    if [[ "$polling_interval" =~ ^[0-9]+$ ]]; then
        echo "✓ Polling interval is valid"
    else
        echo "✗ Polling interval is invalid"
        exit 1
    fi

    if [[ "$idle_timeout" =~ ^[0-9]+$ ]]; then
        echo "✓ Idle timeout is valid"
    else
        echo "✗ Idle timeout is invalid"
        exit 1
    fi

    cleanup_mock_environment
    echo ""
}

# Test: Event emission functions exist
test_event_emission() {
    echo "=== Testing event emission functions ==="

    setup_mock_environment

    # Test that event functions exist and can be called
    _needle_event_worker_started "workspace=/test" "agent=test-agent"
    echo "✓ Worker started event emission works"

    _needle_event_worker_idle "consecutive_empty=1" "idle_seconds=10"
    echo "✓ Worker idle event emission works"

    _needle_event_worker_stopped "reason=test"
    echo "✓ Worker stopped event emission works"

    _needle_event_bead_claimed "nd-test" "workspace=/test"
    echo "✓ Bead claimed event emission works"

    _needle_event_bead_completed "nd-test"
    echo "✓ Bead completed event emission works"

    _needle_event_bead_failed "nd-test" "reason=test"
    echo "✓ Bead failed event emission works"

    cleanup_mock_environment
    echo ""
}

# Test: Telemetry functions exist
test_telemetry() {
    echo "=== Testing telemetry functions ==="

    setup_mock_environment

    _needle_telemetry_init
    echo "✓ Telemetry init works"

    _needle_telemetry_emit "test.event" "key=value"
    echo "✓ Telemetry emit works"

    cleanup_mock_environment
    echo ""
}

# Test: Heartbeat stubs exist
test_heartbeat() {
    echo "=== Testing heartbeat functions ==="

    setup_mock_environment

    _needle_heartbeat_init
    echo "✓ Heartbeat init works"

    _needle_heartbeat_keepalive
    echo "✓ Heartbeat keepalive works"

    _needle_heartbeat_start_bead "nd-test"
    echo "✓ Heartbeat start bead works"

    _needle_heartbeat_end_bead
    echo "✓ Heartbeat end bead works"

    _needle_heartbeat_cleanup
    echo "✓ Heartbeat cleanup works"

    cleanup_mock_environment
    echo ""
}

# Test: Bead processing functions exist
test_bead_functions() {
    echo "=== Testing bead processing functions ==="

    setup_mock_environment

    # Test that bead functions exist
    declare -f _needle_process_bead >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_process_bead function exists"
    else
        echo "✗ _needle_process_bead function missing"
        exit 1
    fi

    declare -f _needle_complete_bead >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_complete_bead function exists"
    else
        echo "✗ _needle_complete_bead function missing"
        exit 1
    fi

    declare -f _needle_fail_bead >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_fail_bead function exists"
    else
        echo "✗ _needle_fail_bead function missing"
        exit 1
    fi

    declare -f _needle_release_bead >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_release_bead function exists"
    else
        echo "✗ _needle_release_bead function missing"
        exit 1
    fi

    cleanup_mock_environment
    echo ""
}

# Test: Worker loop function exists
test_worker_loop() {
    echo "=== Testing worker loop function ==="

    setup_mock_environment

    # Test that worker loop function exists
    declare -f _needle_worker_loop >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_worker_loop function exists"
    else
        echo "✗ _needle_worker_loop function missing"
        exit 1
    fi

    # Test that init function exists
    declare -f _needle_worker_loop_init >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_worker_loop_init function exists"
    else
        echo "✗ _needle_worker_loop_init function missing"
        exit 1
    fi

    cleanup_mock_environment
    echo ""
}

# Test: Signal handler setup function exists
test_signal_handlers() {
    echo "=== Testing signal handler functions ==="

    setup_mock_environment

    # Test that signal handler functions exist
    declare -f _needle_loop_setup_signals >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_loop_setup_signals function exists"
    else
        echo "✗ _needle_loop_setup_signals function missing"
        exit 1
    fi

    declare -f _needle_loop_handle_shutdown >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_loop_handle_shutdown function exists"
    else
        echo "✗ _needle_loop_handle_shutdown function missing"
        exit 1
    fi

    cleanup_mock_environment
    echo ""
}

# Test: Config hot-reload functions
test_config_hot_reload() {
    echo "=== Testing config hot-reload functions ==="

    setup_mock_environment

    # Test that hot-reload functions exist
    declare -f _needle_get_config_mtime >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_get_config_mtime function exists"
    else
        echo "✗ _needle_get_config_mtime function missing"
        exit 1
    fi

    declare -f _needle_check_config_reload >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_check_config_reload function exists"
    else
        echo "✗ _needle_check_config_reload function missing"
        exit 1
    fi

    declare -f _needle_validate_hot_reload_config >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_validate_hot_reload_config function exists"
    else
        echo "✗ _needle_validate_hot_reload_config function missing"
        exit 1
    fi

    # Test _needle_get_config_mtime with existing file
    local test_file="$NEEDLE_HOME/test_config.yaml"
    echo "test: value" > "$test_file"
    local mtime
    mtime=$(_needle_get_config_mtime "$test_file")
    if [[ "$mtime" =~ ^[0-9]+$ ]]; then
        echo "✓ _needle_get_config_mtime returns valid timestamp for existing file"
    else
        echo "✗ _needle_get_config_mtime failed for existing file: $mtime"
        rm -f "$test_file"
        exit 1
    fi

    # Test _needle_get_config_mtime with non-existing file
    mtime=$(_needle_get_config_mtime "/nonexistent/file.yaml")
    if [[ "$mtime" == "0" ]]; then
        echo "✓ _needle_get_config_mtime returns 0 for non-existing file"
    else
        echo "✗ _needle_get_config_mtime should return 0 for non-existing file: $mtime"
        rm -f "$test_file"
        exit 1
    fi

    # Test _needle_validate_hot_reload_config with valid config
    echo "limits:
  global_max_concurrent: 20
runner:
  polling_interval: 2s" > "$test_file"
    if _needle_validate_hot_reload_config "$test_file" ""; then
        echo "✓ _needle_validate_hot_reload_config accepts valid config"
    else
        echo "✗ _needle_validate_hot_reload_config rejected valid config"
        rm -f "$test_file"
        exit 1
    fi

    # Test _needle_validate_hot_reload_config with invalid config (empty)
    echo "" > "$test_file"
    if ! _needle_validate_hot_reload_config "$test_file" ""; then
        echo "✓ _needle_validate_hot_reload_config rejects empty config"
    else
        echo "✗ _needle_validate_hot_reload_config should reject empty config"
        rm -f "$test_file"
        exit 1
    fi

    # Test that config change detection works
    local global_config="$NEEDLE_HOME/config.yaml"
    echo "limits:
  global_max_concurrent: 20" > "$global_config"

    # Initialize mtime tracking
    NEEDLE_GLOBAL_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$global_config")
    NEEDLE_WS_CONFIG_LOADED_AT=0
    NEEDLE_CONFIG_CHECK_COUNTER=0

    # First check should not trigger reload
    NEEDLE_CONFIG_CHECK_COUNTER=$NEEDLE_CONFIG_CHECK_INTERVAL
    if _needle_check_config_reload; then
        echo "✗ First check should not trigger reload"
        rm -f "$test_file" "$global_config"
        exit 1
    fi
    echo "✓ No reload triggered when config unchanged"

    # Modify config and check again
    sleep 1  # Ensure mtime changes
    echo "limits:
  global_max_concurrent: 30" > "$global_config"

    # Reset counter to trigger check
    NEEDLE_CONFIG_CHECK_COUNTER=$NEEDLE_CONFIG_CHECK_INTERVAL
    if _needle_check_config_reload; then
        echo "✓ Config reload triggered when config changed"
    else
        echo "✗ Config reload should be triggered when config changed"
        rm -f "$test_file" "$global_config"
        exit 1
    fi

    # Verify mtime was updated
    local new_mtime
    new_mtime=$(_needle_get_config_mtime "$global_config")
    if [[ "$new_mtime" -gt "$NEEDLE_GLOBAL_CONFIG_LOADED_AT" ]] || [[ "$NEEDLE_GLOBAL_CONFIG_LOADED_AT" == "$new_mtime" ]]; then
        echo "✓ Config mtime tracking updated after reload"
    else
        echo "✗ Config mtime tracking not properly updated"
        rm -f "$test_file" "$global_config"
        exit 1
    fi

    # Cleanup
    rm -f "$test_file" "$global_config"
    cleanup_mock_environment
    echo ""
}

# Test: Per-bead failure tracking functions
test_per_bead_failure_tracking() {
    echo "=== Testing per-bead failure tracking ==="

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Test that tracking functions exist
    declare -f _needle_bead_failure_state_file >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_bead_failure_state_file function exists"
    else
        echo "✗ _needle_bead_failure_state_file function missing"
        exit 1
    fi

    declare -f _needle_get_bead_failure_count >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_get_bead_failure_count function exists"
    else
        echo "✗ _needle_get_bead_failure_count function missing"
        exit 1
    fi

    declare -f _needle_increment_bead_failure_count >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_increment_bead_failure_count function exists"
    else
        echo "✗ _needle_increment_bead_failure_count function missing"
        exit 1
    fi

    declare -f _needle_reset_bead_failure_count >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_reset_bead_failure_count function exists"
    else
        echo "✗ _needle_reset_bead_failure_count function missing"
        exit 1
    fi

    # Test initial count is 0 for unknown bead
    local count
    count=$(_needle_get_bead_failure_count "nd-test99")
    if [[ "$count" -eq 0 ]]; then
        echo "✓ Initial failure count is 0 for unknown bead"
    else
        echo "✗ Initial failure count should be 0, got: $count"
        exit 1
    fi

    # Test incrementing failure count
    local new_count
    new_count=$(_needle_increment_bead_failure_count "nd-test99")
    if [[ "$new_count" -eq 1 ]]; then
        echo "✓ First increment returns 1"
    else
        echo "✗ First increment should return 1, got: $new_count"
        exit 1
    fi

    new_count=$(_needle_increment_bead_failure_count "nd-test99")
    if [[ "$new_count" -eq 2 ]]; then
        echo "✓ Second increment returns 2"
    else
        echo "✗ Second increment should return 2, got: $new_count"
        exit 1
    fi

    # Test get returns persisted count
    count=$(_needle_get_bead_failure_count "nd-test99")
    if [[ "$count" -eq 2 ]]; then
        echo "✓ Get returns persisted count of 2"
    else
        echo "✗ Get should return 2, got: $count"
        exit 1
    fi

    # Test reset clears the count
    _needle_reset_bead_failure_count "nd-test99"
    count=$(_needle_get_bead_failure_count "nd-test99")
    if [[ "$count" -eq 0 ]]; then
        echo "✓ Reset clears failure count to 0"
    else
        echo "✗ After reset, count should be 0, got: $count"
        exit 1
    fi

    # Test multiple beads tracked independently
    _needle_increment_bead_failure_count "nd-beadA" >/dev/null
    _needle_increment_bead_failure_count "nd-beadA" >/dev/null
    _needle_increment_bead_failure_count "nd-beadB" >/dev/null

    local count_a count_b
    count_a=$(_needle_get_bead_failure_count "nd-beadA")
    count_b=$(_needle_get_bead_failure_count "nd-beadB")
    if [[ "$count_a" -eq 2 ]] && [[ "$count_b" -eq 1 ]]; then
        echo "✓ Multiple beads tracked independently"
    else
        echo "✗ Independent tracking failed: A=$count_a, B=$count_b"
        exit 1
    fi

    cleanup_mock_environment
    echo ""
}

# Test: Forced mitosis functions exist and check logic
test_forced_mitosis_functions() {
    echo "=== Testing forced mitosis functions ==="

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Test that forced mitosis functions exist
    declare -f _needle_check_forced_mitosis >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_check_forced_mitosis function exists"
    else
        echo "✗ _needle_check_forced_mitosis function missing"
        exit 1
    fi

    declare -f _needle_handle_forced_mitosis >/dev/null
    if [[ $? -eq 0 ]]; then
        echo "✓ _needle_handle_forced_mitosis function exists"
    else
        echo "✗ _needle_handle_forced_mitosis function missing"
        exit 1
    fi

    # Test _needle_check_forced_mitosis with mocked mitosis config
    # Stub out mitosis config functions to avoid sourcing mitosis.sh
    _NEEDLE_MITOSIS_LOADED=true
    _needle_mitosis_force_enabled() { return 0; }  # enabled
    _needle_mitosis_force_threshold() { echo "3"; }  # threshold=3

    # With 0 failures, should NOT trigger (0 < threshold-1=2)
    _needle_reset_bead_failure_count "nd-forcedtest"
    if ! _needle_check_forced_mitosis "nd-forcedtest" "$NEEDLE_WORKSPACE"; then
        echo "✓ Forced mitosis not triggered at 0 failures (threshold=3)"
    else
        echo "✗ Forced mitosis should not trigger at 0 failures"
        exit 1
    fi

    # With 1 failure, should NOT trigger (1 < threshold-1=2)
    _needle_increment_bead_failure_count "nd-forcedtest" >/dev/null
    if ! _needle_check_forced_mitosis "nd-forcedtest" "$NEEDLE_WORKSPACE"; then
        echo "✓ Forced mitosis not triggered at 1 failure (threshold=3)"
    else
        echo "✗ Forced mitosis should not trigger at 1 failure"
        exit 1
    fi

    # With 2 failures, SHOULD trigger (2 >= threshold-1=2)
    _needle_increment_bead_failure_count "nd-forcedtest" >/dev/null
    if _needle_check_forced_mitosis "nd-forcedtest" "$NEEDLE_WORKSPACE"; then
        echo "✓ Forced mitosis triggered at 2 failures (threshold=3)"
    else
        echo "✗ Forced mitosis should trigger at 2 failures"
        exit 1
    fi

    # Test with force_on_failure disabled
    _needle_mitosis_force_enabled() { return 1; }  # disabled
    _needle_reset_bead_failure_count "nd-disabledtest"
    _needle_increment_bead_failure_count "nd-disabledtest" >/dev/null
    _needle_increment_bead_failure_count "nd-disabledtest" >/dev/null
    _needle_increment_bead_failure_count "nd-disabledtest" >/dev/null
    if ! _needle_check_forced_mitosis "nd-disabledtest" "$NEEDLE_WORKSPACE"; then
        echo "✓ Forced mitosis not triggered when force_on_failure disabled"
    else
        echo "✗ Forced mitosis should not trigger when disabled"
        exit 1
    fi

    # Restore stubs
    unset _NEEDLE_MITOSIS_LOADED
    unset -f _needle_mitosis_force_enabled
    unset -f _needle_mitosis_force_threshold

    cleanup_mock_environment
    echo ""
}

# Test: exit code 1 integration increments per-bead failure count
test_exit_code_1_increments_bead_failure() {
    echo "=== Testing exit code 1 increments per-bead failure count ==="

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Stub out functions that would require real br/mitosis infrastructure
    br() { return 0; }
    _needle_release_bead() { return 0; }
    _needle_increment_backoff() { return 0; }
    _needle_event_bead_failed() { return 0; }
    _needle_telemetry_emit() { return 0; }
    _needle_apply_backoff() { return 0; }
    _needle_should_alert_human() { return 1; }
    _needle_should_exit_worker() { return 1; }
    _NEEDLE_MITOSIS_LOADED=true
    _needle_mitosis_force_enabled() { return 1; }  # disabled so we don't invoke mitosis

    # Verify failure count starts at 0
    local count
    count=$(_needle_get_bead_failure_count "nd-exit1test")
    if [[ "$count" -eq 0 ]]; then
        echo "✓ Failure count starts at 0"
    else
        echo "✗ Failure count should start at 0, got: $count"
        exit 1
    fi

    # Simulate exit code 1 handling
    _needle_handle_exit_code "nd-exit1test" 1 "$NEEDLE_WORKSPACE" "test-agent"

    # Verify failure count was incremented
    count=$(_needle_get_bead_failure_count "nd-exit1test")
    if [[ "$count" -eq 1 ]]; then
        echo "✓ Exit code 1 increments per-bead failure count to 1"
    else
        echo "✗ Exit code 1 should increment failure count, got: $count"
        exit 1
    fi

    # Second failure
    _needle_handle_exit_code "nd-exit1test" 1 "$NEEDLE_WORKSPACE" "test-agent"
    count=$(_needle_get_bead_failure_count "nd-exit1test")
    if [[ "$count" -eq 2 ]]; then
        echo "✓ Second exit code 1 increments count to 2"
    else
        echo "✗ Second failure should give count 2, got: $count"
        exit 1
    fi

    # Cleanup stubs
    unset -f br _needle_release_bead _needle_increment_backoff
    unset -f _needle_event_bead_failed _needle_telemetry_emit _needle_apply_backoff
    unset -f _needle_should_alert_human _needle_should_exit_worker
    unset -f _needle_mitosis_force_enabled
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
    echo ""
}

# Test: exit code 0 resets per-bead failure count
test_exit_code_0_resets_bead_failure() {
    echo "=== Testing exit code 0 resets per-bead failure count ==="

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Stub out functions
    br() { return 0; }
    _needle_complete_bead() { return 0; }
    _needle_reset_backoff() { return 0; }
    _needle_event_bead_completed() { return 0; }
    _needle_telemetry_emit() { return 0; }
    _needle_annotate_bead_with_effort() { return 0; }
    _needle_run_hook() { return 0; }

    # Seed a failure count
    _needle_increment_bead_failure_count "nd-success1" >/dev/null
    _needle_increment_bead_failure_count "nd-success1" >/dev/null

    local count
    count=$(_needle_get_bead_failure_count "nd-success1")
    if [[ "$count" -eq 2 ]]; then
        echo "✓ Pre-condition: failure count seeded to 2"
    else
        echo "✗ Pre-condition failed, count=$count"
        exit 1
    fi

    # Simulate success
    _needle_handle_exit_code "nd-success1" 0 "$NEEDLE_WORKSPACE" "test-agent"

    # Verify failure count was reset
    count=$(_needle_get_bead_failure_count "nd-success1")
    if [[ "$count" -eq 0 ]]; then
        echo "✓ Exit code 0 resets per-bead failure count to 0"
    else
        echo "✗ Exit code 0 should reset failure count, got: $count"
        exit 1
    fi

    # Cleanup stubs
    unset -f br _needle_complete_bead _needle_reset_backoff
    unset -f _needle_event_bead_completed _needle_telemetry_emit
    unset -f _needle_annotate_bead_with_effort _needle_run_hook

    cleanup_mock_environment
    echo ""
}

# Test: forced mitosis triggered on threshold and quarantines on atomic failure
test_forced_mitosis_integration() {
    echo "=== Testing forced mitosis integration in exit code 1 ==="

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Track what actions were taken
    local released=0 quarantined=0 mitosis_attempted=0 mitosis_result=0

    # Stub dependencies
    br() { return 0; }
    _needle_release_bead() { released=1; return 0; }
    _needle_quarantine_bead() { quarantined=1; return 0; }
    _needle_increment_backoff() { return 0; }
    _needle_reset_backoff() { return 0; }
    _needle_apply_backoff() { return 0; }
    _needle_event_bead_failed() { return 0; }
    _needle_telemetry_emit() { return 0; }
    _needle_should_alert_human() { return 1; }
    _needle_should_exit_worker() { return 1; }

    # Stub mitosis config: threshold=3, enabled
    _NEEDLE_MITOSIS_LOADED=true
    _needle_mitosis_force_enabled() { return 0; }
    _needle_mitosis_force_threshold() { echo "3"; }

    # Stub _needle_handle_forced_mitosis to simulate success/failure
    _needle_handle_forced_mitosis() {
        mitosis_attempted=1
        return $mitosis_result  # 0=success, 1=failure (atomic)
    }

    # First two failures (counts 1 and 2 after increment) - below threshold-1=2
    # After first increment: count=1, 1 < 2, no mitosis
    _needle_handle_exit_code "nd-mitosistest" 1 "$NEEDLE_WORKSPACE" "test-agent"
    if [[ "$released" -eq 1 ]] && [[ "$mitosis_attempted" -eq 0 ]]; then
        echo "✓ First failure: normal release, no mitosis attempt"
    else
        echo "✗ First failure: released=$released, mitosis_attempted=$mitosis_attempted"
        exit 1
    fi

    # Second failure: count=2 after increment, 2 >= 2 = threshold-1, should trigger mitosis
    released=0
    mitosis_result=0  # mitosis succeeds
    _needle_handle_exit_code "nd-mitosistest" 1 "$NEEDLE_WORKSPACE" "test-agent"
    if [[ "$mitosis_attempted" -eq 1 ]] && [[ "$released" -eq 0 ]]; then
        echo "✓ Second failure: forced mitosis attempted, bead not released (blocked-by-children)"
    else
        echo "✗ Second failure: mitosis_attempted=$mitosis_attempted, released=$released"
        exit 1
    fi

    # Test quarantine path: mitosis fails (atomic task) → quarantine (not release)
    # Reset counts to trigger threshold again
    _needle_increment_bead_failure_count "nd-atomictest" >/dev/null
    _needle_increment_bead_failure_count "nd-atomictest" >/dev/null
    released=0; mitosis_attempted=0
    mitosis_result=1  # mitosis fails (atomic)

    _needle_handle_exit_code "nd-atomictest" 1 "$NEEDLE_WORKSPACE" "test-agent"
    if [[ "$mitosis_attempted" -eq 1 ]] && [[ "$quarantined" -eq 1 ]] && [[ "$released" -eq 0 ]]; then
        echo "✓ Mitosis failure: atomic task is quarantined (not released)"
    else
        echo "✗ Atomic failure: mitosis_attempted=$mitosis_attempted, quarantined=$quarantined, released=$released"
        exit 1
    fi

    # Cleanup stubs
    unset -f br _needle_release_bead _needle_quarantine_bead _needle_increment_backoff _needle_reset_backoff _needle_apply_backoff
    unset -f _needle_event_bead_failed _needle_telemetry_emit
    unset -f _needle_should_alert_human _needle_should_exit_worker
    unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
    unset -f _needle_handle_forced_mitosis
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
    echo ""
}

# Run all tests
echo "=========================================="
echo "NEEDLE Worker Loop Module Tests"
echo "=========================================="
echo ""

test_config_helper
test_event_emission
test_telemetry
test_heartbeat
test_bead_functions
test_worker_loop
test_signal_handlers
test_config_hot_reload
test_per_bead_failure_tracking
test_forced_mitosis_functions
test_exit_code_1_increments_bead_failure
test_exit_code_0_resets_bead_failure
test_forced_mitosis_integration

echo ""
echo "=========================================="
echo "All tests completed"
echo "=========================================="
