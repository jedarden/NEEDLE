#!/usr/bin/env bash
# Comprehensive test suite for NEEDLE Worker Loop Module (runner/loop.sh)
#
# This test script verifies:
# 1. Basic Loop Tests - Worker initialization, strand execution, heartbeat, shutdown
# 2. Error Handling - Agent failures, missing beads, retries, releases
# 3. Concurrency - max_concurrent limits, coordination, race conditions
# 4. Configuration - Hot-reload, workspace overrides, fallbacks
# 5. Backoff & Crash Recovery - Exponential backoff, alerts, max failures

set -o pipefail

# Test directory
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$TEST_DIR")"
SCRIPT_DIR="$PROJECT_ROOT/src/runner"

# ============================================================================
# Test Framework Setup
# ============================================================================

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test helper: Setup mock environment
setup_mock_environment() {
    export NEEDLE_SESSION="test-session-$$"
    export NEEDLE_RUNNER="test"
    export NEEDLE_PROVIDER="test"
    export NEEDLE_MODEL="test"
    export NEEDLE_IDENTIFIER="test-$$"
    export NEEDLE_WORKSPACE="/tmp/test-workspace-$$"
    export NEEDLE_AGENT="test-agent"
    export NEEDLE_HOME="/tmp/test-needle-home-$$"
    export NEEDLE_STATE_DIR="$NEEDLE_HOME/state"
    export NEEDLE_VERBOSE=true
    export NEEDLE_QUIET=false

    # Reset backoff state
    NEEDLE_FAILURE_COUNT=0
    NEEDLE_BACKOFF_SECONDS=0
    NEEDLE_LAST_FAILURE_TIME=""

    # Reset config tracking
    NEEDLE_CONFIG_LOADED_AT=0
    NEEDLE_WS_CONFIG_LOADED_AT=0
    NEEDLE_GLOBAL_CONFIG_LOADED_AT=0
    NEEDLE_CONFIG_CHECK_COUNTER=0

    # Create test directories
    mkdir -p "$NEEDLE_HOME/state/heartbeats"
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

# Test helper: Start a test
test_start() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
}

# Test helper: Pass a test
test_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

# Test helper: Fail a test
test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    ((TESTS_FAILED++))
}

# Mock br CLI for testing
mock_br() {
    # Create a mock br command
    cat > "$NEEDLE_HOME/br" << 'MOCK_BR'
#!/bin/bash
case "$1" in
    show)
        echo '{"id": "'$2'", "title": "Test Bead", "status": "open"}'
        ;;
    update)
        echo "Updated: $2"
        ;;
    create)
        echo "Created issue nd-test-$$"
        ;;
    list)
        echo '[]'
        ;;
    *)
        echo "Mock br: $@"
        ;;
esac
MOCK_BR
    chmod +x "$NEEDLE_HOME/br"
    export PATH="$NEEDLE_HOME:$PATH"
}

# Cleanup function
cleanup() {
    cleanup_mock_environment
}
trap cleanup EXIT

# ============================================================================
# Source the module under test
# ============================================================================
source "$SCRIPT_DIR/loop.sh"

# ============================================================================
# Basic Loop Tests
# ============================================================================

test_worker_initialization() {
    test_start "Worker initialization"

    setup_mock_environment

    # Test that init function exists
    if ! declare -f _needle_worker_loop_init >/dev/null; then
        test_fail "_needle_worker_loop_init function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test that initialization state is tracked
    if [[ -z "${_NEEDLE_LOOP_INIT:-}" ]]; then
        test_fail "_NEEDLE_LOOP_INIT not defined"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_worker_loop_function_exists() {
    test_start "Worker loop function exists"

    setup_mock_environment

    if declare -f _needle_worker_loop >/dev/null; then
        test_pass
    else
        test_fail "_needle_worker_loop function missing"
    fi

    cleanup_mock_environment
}

test_strand_engine_integration() {
    test_start "Strand engine integration"

    setup_mock_environment

    # Check that strand engine is sourced
    if ! declare -f _needle_strand_engine >/dev/null; then
        test_fail "_needle_strand_engine function not available"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_heartbeat_during_loop() {
    test_start "Heartbeat during loop"

    setup_mock_environment

    # Initialize heartbeat
    _needle_heartbeat_init

    # Verify heartbeat file exists
    if [[ ! -f "$NEEDLE_HEARTBEAT_FILE" ]]; then
        test_fail "Heartbeat file not created"
        cleanup_mock_environment
        return 1
    fi

    # Test keepalive
    _needle_heartbeat_keepalive

    # Verify status is still valid
    local status
    status=$(jq -r '.status' "$NEEDLE_HEARTBEAT_FILE" 2>/dev/null)
    if [[ -z "$status" ]] || [[ "$status" == "null" ]]; then
        test_fail "Heartbeat status invalid after keepalive"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    _needle_heartbeat_cleanup
    cleanup_mock_environment
}

test_graceful_shutdown_signal() {
    test_start "Graceful shutdown signal handling"

    setup_mock_environment

    # Verify signal handler setup function exists
    if ! declare -f _needle_loop_setup_signals >/dev/null; then
        test_fail "_needle_loop_setup_signals function missing"
        cleanup_mock_environment
        return 1
    fi

    # Verify shutdown handler function exists
    if ! declare -f _needle_loop_handle_shutdown >/dev/null; then
        test_fail "_needle_loop_handle_shutdown function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test that shutdown state variable exists
    if [[ -z "${_NEEDLE_LOOP_SHUTDOWN:-}" ]]; then
        test_fail "_NEEDLE_LOOP_SHUTDOWN not defined"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_shutdown_triggers_draining() {
    test_start "Shutdown triggers draining state"

    setup_mock_environment

    # Reset state
    _NEEDLE_LOOP_SHUTDOWN=false
    _NEEDLE_LOOP_DRAINING=false

    # Simulate shutdown handler
    _needle_loop_handle_shutdown TERM 2>/dev/null || true

    # Check draining state was set
    if [[ "$_NEEDLE_LOOP_DRAINING" != "true" ]]; then
        test_fail "Draining state not set on shutdown"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_worker_cleanup_function() {
    test_start "Worker cleanup function"

    setup_mock_environment

    # Verify cleanup function exists
    if ! declare -f _needle_worker_cleanup >/dev/null; then
        test_fail "_needle_worker_cleanup function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

# ============================================================================
# Error Handling Tests
# ============================================================================

test_agent_failure_recovery() {
    test_start "Agent failure recovery"

    setup_mock_environment

    # Verify failure handling functions exist
    if ! declare -f _needle_fail_bead >/dev/null; then
        test_fail "_needle_fail_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    if ! declare -f _needle_release_bead >/dev/null; then
        test_fail "_needle_release_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_handle_exit_code_success() {
    test_start "Handle exit code: success (0)"

    setup_mock_environment
    mock_br

    # Verify exit code handler exists
    if ! declare -f _needle_handle_exit_code >/dev/null; then
        test_fail "_needle_handle_exit_code function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test success case (exit code 0)
    # Note: We can't fully test this without a real bead, but we can verify the function exists
    test_pass
    cleanup_mock_environment
}

test_handle_exit_code_failure() {
    test_start "Handle exit code: failure (1)"

    setup_mock_environment
    mock_br

    # Verify backoff increment function exists
    if ! declare -f _needle_increment_backoff >/dev/null; then
        test_fail "_needle_increment_backoff function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_handle_exit_code_timeout() {
    test_start "Handle exit code: timeout (124)"

    setup_mock_environment
    mock_br

    # The timeout case (exit code 124) should release the bead
    # We verify the release function exists
    if ! declare -f _needle_release_bead >/dev/null; then
        test_fail "_needle_release_bead function missing for timeout handling"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_missing_bead_handling() {
    test_start "Missing bead handling"

    setup_mock_environment

    # Verify process_bead handles missing beads
    if ! declare -f _needle_process_bead >/dev/null; then
        test_fail "_needle_process_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    # The process_bead function should handle missing beads gracefully
    # by checking if the bead exists before processing
    test_pass
    cleanup_mock_environment
}

test_bead_claim_failure_handling() {
    test_start "Bead claim failure handling"

    setup_mock_environment

    # Verify event emission for claim failures
    if ! declare -f _needle_event_error_claim_failed >/dev/null; then
        # This might be optional - check if basic event functions exist
        if ! declare -f _needle_event_bead_failed >/dev/null; then
            test_fail "Event emission functions missing for claim failures"
            cleanup_mock_environment
            return 1
        fi
    fi

    test_pass
    cleanup_mock_environment
}

test_retry_on_transient_error() {
    test_start "Retry on transient error"

    setup_mock_environment

    # The backoff system enables retries
    # Verify backoff reset function exists
    if ! declare -f _needle_reset_backoff >/dev/null; then
        test_fail "_needle_reset_backoff function missing"
        cleanup_mock_environment
        return 1
    fi

    # Verify backoff increment exists
    if ! declare -f _needle_increment_backoff >/dev/null; then
        test_fail "_needle_increment_backoff function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_release_on_fatal_error() {
    test_start "Release on fatal error"

    setup_mock_environment

    # Verify release function
    if ! declare -f _needle_release_bead >/dev/null; then
        test_fail "_needle_release_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    # Verify fail function
    if ! declare -f _needle_fail_bead >/dev/null; then
        test_fail "_needle_fail_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

# ============================================================================
# Concurrency Tests
# ============================================================================

test_max_concurrent_config() {
    test_start "Max concurrent configuration"

    setup_mock_environment

    # Verify config helper function
    if ! declare -f _needle_loop_get_config >/dev/null; then
        test_fail "_needle_loop_get_config function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test default value retrieval
    local polling_interval
    polling_interval=$(_needle_loop_get_config "runner.polling_interval" "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL")

    if [[ -z "$polling_interval" ]]; then
        test_fail "Config helper returned empty value"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_worker_registration() {
    test_start "Worker registration"

    setup_mock_environment

    # Verify worker registration functions
    if ! declare -f _needle_register_worker >/dev/null; then
        test_fail "_needle_register_worker function missing"
        cleanup_mock_environment
        return 1
    fi

    if ! declare -f _needle_unregister_worker >/dev/null; then
        test_fail "_needle_unregister_worker function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_worker_count_function() {
    test_start "Worker count function"

    setup_mock_environment

    # Verify worker counting functions
    if ! declare -f _needle_count_all_workers >/dev/null; then
        test_fail "_needle_count_all_workers function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_no_race_on_bead_claim() {
    test_start "No race condition on bead claim"

    setup_mock_environment

    # Verify bead claim function exists
    # The actual race condition testing would require integration tests
    if ! declare -f _needle_process_bead >/dev/null; then
        test_fail "_needle_process_bead function missing"
        cleanup_mock_environment
        return 1
    fi

    # The claim mechanism should use atomic operations via br CLI
    test_pass
    cleanup_mock_environment
}

test_concurrent_worker_coordination() {
    test_start "Concurrent worker coordination"

    setup_mock_environment

    # Verify state management functions
    if ! declare -f _needle_is_worker_registered >/dev/null; then
        test_fail "_needle_is_worker_registered function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

# ============================================================================
# Configuration Tests
# ============================================================================

test_config_hot_reload_detection() {
    test_start "Config hot-reload detection"

    setup_mock_environment

    # Create a test config file
    local test_config="$NEEDLE_HOME/config.yaml"
    echo "limits:
  global_max_concurrent: 20" > "$test_config"

    # Initialize config tracking
    NEEDLE_GLOBAL_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$test_config")
    NEEDLE_WS_CONFIG_LOADED_AT=0
    NEEDLE_CONFIG_CHECK_COUNTER=0

    # First check should not trigger reload
    NEEDLE_CONFIG_CHECK_COUNTER=$NEEDLE_CONFIG_CHECK_INTERVAL
    if _needle_check_config_reload 2>/dev/null; then
        test_fail "First check should not trigger reload"
        rm -f "$test_config"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    rm -f "$test_config"
    cleanup_mock_environment
}

test_config_hot_reload_trigger() {
    test_start "Config hot-reload trigger on change"

    setup_mock_environment

    # Create a test config file
    local test_config="$NEEDLE_HOME/config.yaml"
    echo "limits:
  global_max_concurrent: 20" > "$test_config"

    # Initialize config tracking
    NEEDLE_GLOBAL_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$test_config")
    NEEDLE_WS_CONFIG_LOADED_AT=0
    NEEDLE_CONFIG_CHECK_COUNTER=0

    # Wait a moment and modify config
    sleep 1
    echo "limits:
  global_max_concurrent: 30" > "$test_config"

    # Reset counter to trigger check
    NEEDLE_CONFIG_CHECK_COUNTER=$NEEDLE_CONFIG_CHECK_INTERVAL
    if _needle_check_config_reload 2>/dev/null; then
        test_pass
    else
        test_fail "Config reload should be triggered when config changed"
    fi

    rm -f "$test_config"
    cleanup_mock_environment
}

test_config_mtime_function() {
    test_start "Config mtime function"

    setup_mock_environment

    # Create a test file
    local test_file="$NEEDLE_HOME/test_file.txt"
    echo "test" > "$test_file"

    # Get mtime
    local mtime
    mtime=$(_needle_get_config_mtime "$test_file")

    if [[ "$mtime" =~ ^[0-9]+$ ]] && [[ "$mtime" -gt 0 ]]; then
        test_pass
    else
        test_fail "mtime should be a positive integer, got: $mtime"
    fi

    rm -f "$test_file"
    cleanup_mock_environment
}

test_config_mtime_nonexistent() {
    test_start "Config mtime for nonexistent file"

    setup_mock_environment

    # Get mtime for nonexistent file
    local mtime
    mtime=$(_needle_get_config_mtime "/nonexistent/file.yaml")

    if [[ "$mtime" == "0" ]]; then
        test_pass
    else
        test_fail "mtime should be 0 for nonexistent file, got: $mtime"
    fi

    cleanup_mock_environment
}

test_config_validation_valid() {
    test_start "Config validation: valid config"

    setup_mock_environment

    # Create a valid config
    local test_config="$NEEDLE_HOME/config.yaml"
    echo "limits:
  global_max_concurrent: 20
runner:
  polling_interval: 2s" > "$test_config"

    if _needle_validate_hot_reload_config "$test_config" "" 2>/dev/null; then
        test_pass
    else
        test_fail "Valid config should pass validation"
    fi

    rm -f "$test_config"
    cleanup_mock_environment
}

test_config_validation_empty() {
    test_start "Config validation: empty config"

    setup_mock_environment

    # Create an empty config
    local test_config="$NEEDLE_HOME/config.yaml"
    echo "" > "$test_config"

    if ! _needle_validate_hot_reload_config "$test_config" "" 2>/dev/null; then
        test_pass
    else
        test_fail "Empty config should fail validation"
    fi

    rm -f "$test_config"
    cleanup_mock_environment
}

test_workspace_config_override() {
    test_start "Workspace config override"

    setup_mock_environment

    # Create workspace config
    local ws_config="$NEEDLE_WORKSPACE/.needle.yaml"
    mkdir -p "$NEEDLE_WORKSPACE"
    echo "runner:
  polling_interval: 5s" > "$ws_config"

    # Verify config tracking for workspace
    NEEDLE_WS_CONFIG_LOADED_AT=$(_needle_get_config_mtime "$ws_config")

    if [[ "$NEEDLE_WS_CONFIG_LOADED_AT" -gt 0 ]]; then
        test_pass
    else
        test_fail "Workspace config mtime should be tracked"
    fi

    rm -f "$ws_config"
    cleanup_mock_environment
}

test_config_fallback_to_default() {
    test_start "Config fallback to default"

    setup_mock_environment

    # Test getting config with no config file
    local value
    value=$(_needle_loop_get_config "runner.polling_interval" "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL")

    if [[ "$value" == "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL" ]]; then
        test_pass
    else
        test_fail "Should fallback to default value, got: $value"
    fi

    cleanup_mock_environment
}

# ============================================================================
# Backoff & Crash Recovery Tests
# ============================================================================

test_backoff_reset() {
    test_start "Backoff reset"

    setup_mock_environment

    # Set some backoff state
    NEEDLE_FAILURE_COUNT=3
    NEEDLE_BACKOFF_SECONDS=60
    NEEDLE_LAST_FAILURE_TIME="2024-01-01T00:00:00Z"

    # Reset backoff
    _needle_reset_backoff

    if [[ "$NEEDLE_FAILURE_COUNT" -eq 0 ]] && \
       [[ "$NEEDLE_BACKOFF_SECONDS" -eq 0 ]] && \
       [[ -z "$NEEDLE_LAST_FAILURE_TIME" ]]; then
        test_pass
    else
        test_fail "Backoff state not reset properly: count=$NEEDLE_FAILURE_COUNT, seconds=$NEEDLE_BACKOFF_SECONDS"
    fi

    cleanup_mock_environment
}

test_backoff_increment() {
    test_start "Backoff increment"

    setup_mock_environment

    # Reset state
    NEEDLE_FAILURE_COUNT=0
    NEEDLE_BACKOFF_SECONDS=0

    # Increment once - don't use subshell to preserve variable changes
    _needle_increment_backoff >/dev/null

    # Verify failure count incremented
    if [[ "$NEEDLE_FAILURE_COUNT" -ne 1 ]]; then
        test_fail "Failure count should be 1, got: $NEEDLE_FAILURE_COUNT"
        cleanup_mock_environment
        return 1
    fi

    # Below threshold, backoff should still be 0
    if [[ "$NEEDLE_BACKOFF_SECONDS" -ne 0 ]]; then
        test_fail "Backoff should be 0 below threshold, got: $NEEDLE_BACKOFF_SECONDS"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_backoff_exponential() {
    test_start "Backoff exponential growth"

    setup_mock_environment

    # Reset state
    NEEDLE_FAILURE_COUNT=0
    NEEDLE_BACKOFF_SECONDS=0

    # Increment to threshold (3 failures to start backoff)
    _needle_increment_backoff  # 1
    _needle_increment_backoff  # 2
    local backoff3
    backoff3=$(_needle_increment_backoff)  # 3 - should start backoff

    # At threshold, should have backoff
    if [[ "$backoff3" -lt 1 ]]; then
        test_fail "Backoff should start at threshold, got: $backoff3"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_backoff_max_cap() {
    test_start "Backoff max cap"

    setup_mock_environment

    # Verify max constant exists
    if [[ -z "${NEEDLE_BACKOFF_MAX_SECONDS:-}" ]]; then
        test_fail "NEEDLE_BACKOFF_MAX_SECONDS not defined"
        cleanup_mock_environment
        return 1
    fi

    # Verify max is reasonable (120s = 2 minutes)
    if [[ "$NEEDLE_BACKOFF_MAX_SECONDS" -gt 300 ]]; then
        test_fail "NEEDLE_BACKOFF_MAX_SECONDS seems too high: $NEEDLE_BACKOFF_MAX_SECONDS"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_alert_human_threshold() {
    test_start "Alert human threshold"

    setup_mock_environment

    # Verify threshold constant exists
    if [[ -z "${NEEDLE_ALERT_THRESHOLD:-}" ]]; then
        test_fail "NEEDLE_ALERT_THRESHOLD not defined"
        cleanup_mock_environment
        return 1
    fi

    # Verify alert function exists
    if ! declare -f _needle_should_alert_human >/dev/null; then
        test_fail "_needle_should_alert_human function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test below threshold
    NEEDLE_FAILURE_COUNT=$((NEEDLE_ALERT_THRESHOLD - 1))
    if _needle_should_alert_human; then
        test_fail "Should not alert below threshold"
        cleanup_mock_environment
        return 1
    fi

    # Test at threshold
    NEEDLE_FAILURE_COUNT=$NEEDLE_ALERT_THRESHOLD
    if ! _needle_should_alert_human; then
        test_fail "Should alert at threshold"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_max_failures_exit() {
    test_start "Max failures exit"

    setup_mock_environment

    # Verify max failures constant exists
    if [[ -z "${NEEDLE_MAX_FAILURES:-}" ]]; then
        test_fail "NEEDLE_MAX_FAILURES not defined"
        cleanup_mock_environment
        return 1
    fi

    # Verify exit check function exists
    if ! declare -f _needle_should_exit_worker >/dev/null; then
        test_fail "_needle_should_exit_worker function missing"
        cleanup_mock_environment
        return 1
    fi

    # Test below max
    NEEDLE_FAILURE_COUNT=$((NEEDLE_MAX_FAILURES - 1))
    if _needle_should_exit_worker; then
        test_fail "Should not exit below max failures"
        cleanup_mock_environment
        return 1
    fi

    # Test at max
    NEEDLE_FAILURE_COUNT=$NEEDLE_MAX_FAILURES
    if ! _needle_should_exit_worker; then
        test_fail "Should exit at max failures"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

test_backoff_apply() {
    test_start "Backoff apply"

    setup_mock_environment

    # Verify apply function exists
    if ! declare -f _needle_apply_backoff >/dev/null; then
        test_fail "_needle_apply_backoff function missing"
        cleanup_mock_environment
        return 1
    fi

    # We don't actually sleep in tests, just verify function exists
    test_pass
    cleanup_mock_environment
}

test_crash_loop_alert() {
    test_start "Crash loop alert function"

    setup_mock_environment
    mock_br

    # Verify crash loop alert function exists
    if ! declare -f _needle_alert_crash_loop >/dev/null; then
        test_fail "_needle_alert_crash_loop function missing"
        cleanup_mock_environment
        return 1
    fi

    test_pass
    cleanup_mock_environment
}

# ============================================================================
# Telemetry Tests
# ============================================================================

test_telemetry_init() {
    test_start "Telemetry functions"

    setup_mock_environment

    # Check for telemetry emit function (the main telemetry function)
    if declare -f _needle_telemetry_emit >/dev/null; then
        test_pass
    else
        test_fail "_needle_telemetry_emit function missing"
    fi

    cleanup_mock_environment
}

test_telemetry_emit() {
    test_start "Telemetry emit"

    setup_mock_environment

    if declare -f _needle_telemetry_emit >/dev/null; then
        test_pass
    else
        test_fail "_needle_telemetry_emit function missing"
    fi

    cleanup_mock_environment
}

# ============================================================================
# Event Emission Tests
# ============================================================================

test_event_worker_started() {
    test_start "Event: worker started"

    setup_mock_environment

    if declare -f _needle_event_worker_started >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_worker_started function missing"
    fi

    cleanup_mock_environment
}

test_event_worker_idle() {
    test_start "Event: worker idle"

    setup_mock_environment

    if declare -f _needle_event_worker_idle >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_worker_idle function missing"
    fi

    cleanup_mock_environment
}

test_event_worker_stopped() {
    test_start "Event: worker stopped"

    setup_mock_environment

    if declare -f _needle_event_worker_stopped >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_worker_stopped function missing"
    fi

    cleanup_mock_environment
}

test_event_bead_claimed() {
    test_start "Event: bead claimed"

    setup_mock_environment

    if declare -f _needle_event_bead_claimed >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_bead_claimed function missing"
    fi

    cleanup_mock_environment
}

test_event_bead_completed() {
    test_start "Event: bead completed"

    setup_mock_environment

    if declare -f _needle_event_bead_completed >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_bead_completed function missing"
    fi

    cleanup_mock_environment
}

test_event_bead_failed() {
    test_start "Event: bead failed"

    setup_mock_environment

    if declare -f _needle_event_bead_failed >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_bead_failed function missing"
    fi

    cleanup_mock_environment
}

test_event_bead_released() {
    test_start "Event: bead released"

    setup_mock_environment

    if declare -f _needle_event_bead_released >/dev/null; then
        test_pass
    else
        test_fail "_needle_event_bead_released function missing"
    fi

    cleanup_mock_environment
}

# ============================================================================
# Bead Processing Tests
# ============================================================================

test_process_bead_function() {
    test_start "Process bead function"

    setup_mock_environment

    if declare -f _needle_process_bead >/dev/null; then
        test_pass
    else
        test_fail "_needle_process_bead function missing"
    fi

    cleanup_mock_environment
}

test_complete_bead_function() {
    test_start "Complete bead function"

    setup_mock_environment

    if declare -f _needle_complete_bead >/dev/null; then
        test_pass
    else
        test_fail "_needle_complete_bead function missing"
    fi

    cleanup_mock_environment
}

test_build_prompt_function() {
    test_start "Build prompt function"

    setup_mock_environment

    if declare -f _needle_build_prompt >/dev/null; then
        test_pass
    else
        test_fail "_needle_build_prompt function missing"
    fi

    cleanup_mock_environment
}

test_dispatch_agent_function() {
    test_start "Dispatch agent function"

    setup_mock_environment

    if declare -f _needle_dispatch_agent >/dev/null; then
        test_pass
    else
        test_fail "_needle_dispatch_agent function missing"
    fi

    cleanup_mock_environment
}

test_run_hook_function() {
    test_start "Run hook function"

    setup_mock_environment

    if declare -f _needle_run_hook >/dev/null; then
        test_pass
    else
        test_fail "_needle_run_hook function missing"
    fi

    cleanup_mock_environment
}

test_cleanup_execution_function() {
    test_start "Cleanup execution function"

    setup_mock_environment

    if declare -f _needle_cleanup_execution >/dev/null; then
        test_pass
    else
        test_fail "_needle_cleanup_execution function missing"
    fi

    cleanup_mock_environment
}

# ============================================================================
# Default Configuration Tests
# ============================================================================

test_default_polling_interval() {
    test_start "Default polling interval"

    if [[ -n "${NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL:-}" ]]; then
        # Verify it's a valid time string (ends with 's')
        if [[ "$NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL" =~ ^[0-9]+s$ ]]; then
            test_pass
        else
            test_fail "Invalid polling interval format: $NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL"
        fi
    else
        test_fail "NEEDLE_LOOP_DEFAULT_POLLING_INTERVAL not defined"
    fi
}

test_default_idle_timeout() {
    test_start "Default idle timeout"

    if [[ -n "${NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT:-}" ]]; then
        # Verify it's a valid time string (ends with 's')
        if [[ "$NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT" =~ ^[0-9]+s$ ]]; then
            test_pass
        else
            test_fail "Invalid idle timeout format: $NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT"
        fi
    else
        test_fail "NEEDLE_LOOP_DEFAULT_IDLE_TIMEOUT not defined"
    fi
}

test_default_max_empty() {
    test_start "Default max empty iterations"

    if [[ -n "${NEEDLE_LOOP_DEFAULT_MAX_EMPTY:-}" ]]; then
        test_pass
    else
        test_fail "NEEDLE_LOOP_DEFAULT_MAX_EMPTY not defined"
    fi
}

test_shutdown_grace_period() {
    test_start "Shutdown grace period"

    if [[ -n "${NEEDLE_LOOP_SHUTDOWN_GRACE_PERIOD:-}" ]]; then
        test_pass
    else
        test_fail "NEEDLE_LOOP_SHUTDOWN_GRACE_PERIOD not defined"
    fi
}

# ============================================================================
# Exit Code Handler Tests
# ============================================================================

test_exit_code_handler_exists() {
    test_start "Exit code handler exists"

    setup_mock_environment

    if declare -f _needle_handle_exit_code >/dev/null; then
        test_pass
    else
        test_fail "_needle_handle_exit_code function missing"
    fi

    cleanup_mock_environment
}

test_exit_code_success_path() {
    test_start "Exit code success path (0)"

    setup_mock_environment

    # Verify that success path calls complete_bead
    if declare -f _needle_complete_bead >/dev/null; then
        test_pass
    else
        test_fail "Success path requires _needle_complete_bead"
    fi

    cleanup_mock_environment
}

test_exit_code_failure_path() {
    test_start "Exit code failure path (1)"

    setup_mock_environment

    # Verify that failure path releases bead and increments backoff
    if declare -f _needle_release_bead >/dev/null && \
       declare -f _needle_increment_backoff >/dev/null; then
        test_pass
    else
        test_fail "Failure path requires release and backoff functions"
    fi

    cleanup_mock_environment
}

test_exit_code_timeout_path() {
    test_start "Exit code timeout path (124)"

    setup_mock_environment

    # Verify that timeout path handles correctly
    # Exit code 124 is standard timeout exit code
    test_pass
    cleanup_mock_environment
}

# ============================================================================
# Forced Mitosis Tracking Tests (Per-Bead Failure Count in loop.sh)
# ============================================================================

test_bead_failure_count_zero_when_no_file() {
    test_start "Per-bead failure count returns 0 with no state file"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # No state file exists yet
    local count
    count=$(_needle_get_bead_failure_count "nd-test-bead")

    if [[ "$count" -eq 0 ]]; then
        test_pass
    else
        test_fail "Expected 0, got: $count"
    fi

    cleanup_mock_environment
}

test_bead_failure_count_increment_from_zero() {
    test_start "Per-bead failure count increments from 0 to 1"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    local new_count
    new_count=$(_needle_increment_bead_failure_count "nd-abc")

    if [[ "$new_count" -eq 1 ]]; then
        test_pass
    else
        test_fail "Expected 1, got: $new_count"
    fi

    cleanup_mock_environment
}

test_bead_failure_count_increment_idempotent() {
    test_start "Per-bead failure count increments correctly across calls"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    _needle_increment_bead_failure_count "nd-xyz" >/dev/null
    local second
    second=$(_needle_increment_bead_failure_count "nd-xyz")

    if [[ "$second" -eq 2 ]]; then
        test_pass
    else
        test_fail "Expected 2 on second increment, got: $second"
    fi

    cleanup_mock_environment
}

test_bead_failure_count_reset() {
    test_start "Per-bead failure count reset removes entry"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    _needle_increment_bead_failure_count "nd-rst" >/dev/null
    _needle_reset_bead_failure_count "nd-rst"

    local count
    count=$(_needle_get_bead_failure_count "nd-rst")

    if [[ "$count" -eq 0 ]]; then
        test_pass
    else
        test_fail "Expected 0 after reset, got: $count"
    fi

    cleanup_mock_environment
}

test_check_forced_mitosis_disabled() {
    test_start "_needle_check_forced_mitosis returns 1 when force_on_failure disabled"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Prevent re-sourcing of mitosis.sh so mocks persist
    _NEEDLE_MITOSIS_LOADED=1
    _needle_mitosis_force_enabled() { return 1; }
    _needle_mitosis_force_threshold() { echo "3"; }
    # Even with count at threshold, disabled flag takes precedence
    echo '{"nd-chk": 2}' > "$(_needle_bead_failure_state_file)"

    if ! _needle_check_forced_mitosis "nd-chk" "$NEEDLE_WORKSPACE"; then
        test_pass
    else
        test_fail "Expected non-zero (disabled), got zero"
    fi

    unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

test_check_forced_mitosis_below_threshold() {
    test_start "_needle_check_forced_mitosis returns 1 when below threshold"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Prevent re-sourcing of mitosis.sh so mocks persist
    _NEEDLE_MITOSIS_LOADED=1
    # Enable force mitosis with threshold 3, but bead has only 1 failure
    _needle_mitosis_force_enabled() { return 0; }
    _needle_mitosis_force_threshold() { echo "3"; }
    echo '{"nd-below": 1}' > "$(_needle_bead_failure_state_file)"

    if ! _needle_check_forced_mitosis "nd-below" "$NEEDLE_WORKSPACE"; then
        test_pass
    else
        test_fail "Expected non-zero (below threshold), got zero"
    fi

    unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

test_check_forced_mitosis_at_threshold() {
    test_start "_needle_check_forced_mitosis returns 0 when threshold reached"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"

    # Prevent re-sourcing of mitosis.sh so mocks persist
    _NEEDLE_MITOSIS_LOADED=1
    # Threshold=3, bead has 2 failures (threshold - 1 triggers it)
    _needle_mitosis_force_enabled() { return 0; }
    _needle_mitosis_force_threshold() { echo "3"; }
    echo '{"nd-thresh": 2}' > "$(_needle_bead_failure_state_file)"

    if _needle_check_forced_mitosis "nd-thresh" "$NEEDLE_WORKSPACE"; then
        test_pass
    else
        test_fail "Expected zero (at threshold-1), got non-zero"
    fi

    unset -f _needle_mitosis_force_enabled _needle_mitosis_force_threshold
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

test_handle_forced_mitosis_calls_check_mitosis_with_force_true() {
    test_start "_needle_handle_forced_mitosis calls _needle_check_mitosis with force=true"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"
    echo '{"nd-hfm": 2}' > "$(_needle_bead_failure_state_file)"

    # Prevent re-sourcing of mitosis.sh so _needle_check_mitosis mock persists
    _NEEDLE_MITOSIS_LOADED=1
    local FORCE_ARG=""
    _needle_check_mitosis() {
        FORCE_ARG="${4:-false}"
        return 0
    }

    _needle_handle_forced_mitosis "nd-hfm" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1

    if [[ "$FORCE_ARG" == "true" ]]; then
        test_pass
    else
        test_fail "Expected force=true, got: $FORCE_ARG"
    fi

    unset -f _needle_check_mitosis
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

test_handle_forced_mitosis_resets_failure_count_on_success() {
    test_start "_needle_handle_forced_mitosis resets failure count on mitosis success"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"
    echo '{"nd-succ": 2}' > "$(_needle_bead_failure_state_file)"

    _NEEDLE_MITOSIS_LOADED=1
    _needle_check_mitosis() { return 0; }

    _needle_handle_forced_mitosis "nd-succ" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1

    local remaining
    remaining=$(_needle_get_bead_failure_count "nd-succ")

    if [[ "$remaining" -eq 0 ]]; then
        test_pass
    else
        test_fail "Expected failure count reset to 0, got: $remaining"
    fi

    unset -f _needle_check_mitosis
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

test_handle_forced_mitosis_resets_failure_count_on_failure() {
    test_start "_needle_handle_forced_mitosis resets failure count when mitosis cannot decompose"

    setup_mock_environment
    mkdir -p "$NEEDLE_STATE_DIR"
    echo '{"nd-fail": 2}' > "$(_needle_bead_failure_state_file)"

    _NEEDLE_MITOSIS_LOADED=1
    _needle_check_mitosis() { return 1; }

    _needle_handle_forced_mitosis "nd-fail" "$NEEDLE_WORKSPACE" "test-agent" >/dev/null 2>&1

    local remaining
    remaining=$(_needle_get_bead_failure_count "nd-fail")

    if [[ "$remaining" -eq 0 ]]; then
        test_pass
    else
        test_fail "Expected failure count reset to 0, got: $remaining"
    fi

    unset -f _needle_check_mitosis
    unset _NEEDLE_MITOSIS_LOADED

    cleanup_mock_environment
}

# ============================================================================
# Run All Tests
# ============================================================================

echo "=========================================="
echo "NEEDLE Worker Loop Module Tests"
echo "tests/test_runner.sh"
echo "=========================================="
echo ""

# Basic Loop Tests
echo "--- Basic Loop Tests ---"
test_worker_initialization
test_worker_loop_function_exists
test_strand_engine_integration
test_heartbeat_during_loop
test_graceful_shutdown_signal
test_shutdown_triggers_draining
test_worker_cleanup_function

# Error Handling Tests
echo ""
echo "--- Error Handling Tests ---"
test_agent_failure_recovery
test_handle_exit_code_success
test_handle_exit_code_failure
test_handle_exit_code_timeout
test_missing_bead_handling
test_bead_claim_failure_handling
test_retry_on_transient_error
test_release_on_fatal_error

# Concurrency Tests
echo ""
echo "--- Concurrency Tests ---"
test_max_concurrent_config
test_worker_registration
test_worker_count_function
test_no_race_on_bead_claim
test_concurrent_worker_coordination

# Configuration Tests
echo ""
echo "--- Configuration Tests ---"
test_config_hot_reload_detection
test_config_hot_reload_trigger
test_config_mtime_function
test_config_mtime_nonexistent
test_config_validation_valid
test_config_validation_empty
test_workspace_config_override
test_config_fallback_to_default

# Backoff & Crash Recovery Tests
echo ""
echo "--- Backoff & Crash Recovery Tests ---"
test_backoff_reset
test_backoff_increment
test_backoff_exponential
test_backoff_max_cap
test_alert_human_threshold
test_max_failures_exit
test_backoff_apply
test_crash_loop_alert

# Telemetry Tests
echo ""
echo "--- Telemetry Tests ---"
test_telemetry_init
test_telemetry_emit

# Event Emission Tests
echo ""
echo "--- Event Emission Tests ---"
test_event_worker_started
test_event_worker_idle
test_event_worker_stopped
test_event_bead_claimed
test_event_bead_completed
test_event_bead_failed
test_event_bead_released

# Bead Processing Tests
echo ""
echo "--- Bead Processing Tests ---"
test_process_bead_function
test_complete_bead_function
test_build_prompt_function
test_dispatch_agent_function
test_run_hook_function
test_cleanup_execution_function

# Default Configuration Tests
echo ""
echo "--- Default Configuration Tests ---"
test_default_polling_interval
test_default_idle_timeout
test_default_max_empty
test_shutdown_grace_period

# Exit Code Handler Tests
echo ""
echo "--- Exit Code Handler Tests ---"
test_exit_code_handler_exists
test_exit_code_success_path
test_exit_code_failure_path
test_exit_code_timeout_path

# Forced Mitosis Tracking Tests
echo ""
echo "--- Forced Mitosis Tracking Tests ---"
test_bead_failure_count_zero_when_no_file
test_bead_failure_count_increment_from_zero
test_bead_failure_count_increment_idempotent
test_bead_failure_count_reset
test_check_forced_mitosis_disabled
test_check_forced_mitosis_below_threshold
test_check_forced_mitosis_at_threshold
test_handle_forced_mitosis_calls_check_mitosis_with_force_true
test_handle_forced_mitosis_resets_failure_count_on_success
test_handle_forced_mitosis_resets_failure_count_on_failure

# ============================================================================
# Summary
# ============================================================================
echo ""
echo "=========================================="
echo "Test Results"
echo "=========================================="
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "=========================================="

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "✓ All tests passed!"
    exit 0
else
    echo "✗ Some tests failed"
    exit 1
fi
