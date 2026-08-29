#!/usr/bin/env bash
#
# Routing Test Helpers for NEEDLE
#
# Reusable shell functions for testing model-based adapter routing verification.
# Source this script in your test scripts to use the helper functions.
#
# Usage:
#   source tests/routing-test-helpers.sh
#   dispatch_test_bead "my-test" "claude-sonnet-4-6" "claude-print"
#
# Requirements:
#   - bead CLI (bead-rs backend)
#   - jq for JSON parsing
#   - needle binary in PATH
#

set -euo pipefail

# ============================================================================
# CONSTANTS AND CONFIGURATION
# ============================================================================

# Colors for output
readonly GREEN='\033[0;32m'
readonly RED='\033[0;31m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly NC='\033[0m' # No Color

# Test directories and prefixes
readonly TEST_BEAD_PREFIX="route-test"
readonly TEST_WORKSPACE_ROOT="${TEST_WORKSPACE_ROOT:-/tmp/needle-routing-tests-$$}"
readonly TEST_TIMEOUT_SECS="${TEST_TIMEOUT_SECS:-600}"

# Bead statuses
readonly BEAD_STATUS_OPEN="open"
readonly BEAD_STATUS_IN_PROGRESS="in_progress"
readonly BEAD_STATUS_CLOSED="closed"
readonly BEAD_STATUS_DEFERRED="deferred"

# ============================================================================
# LOGGING FUNCTIONS
# ============================================================================

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_debug() {
    if [[ "${DEBUG:-0}" == "1" ]]; then
        echo -e "${BLUE}[DEBUG]${NC} $1"
    fi
}

log_section() {
    local title="$1"
    local width=60
    local padding=$(( (width - ${#title} - 2) / 2 ))
    printf "%${padding}s" | tr ' ' '='
    echo -n " $title "
    printf "%${padding}s" | tr ' ' '='
    echo
}

# ============================================================================
# PREREQUISITE CHECKING
# ============================================================================

check_prerequisites() {
    log_info "Checking prerequisites..."

    local missing=()

    # Check for bead CLI
    if ! command -v bead &> /dev/null; then
        missing+=("bead CLI")
    fi

    # Check for jq
    if ! command -v jq &> /dev/null; then
        missing+=("jq")
    fi

    # Check for needle binary
    if ! command -v needle &> /dev/null; then
        missing+=("needle binary")
    fi

    # Check for git (required for bead operations)
    if ! command -v git &> /dev/null; then
        missing+=("git")
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing required tools: ${missing[*]}"
        return 1
    fi

    log_info "✓ All prerequisites met"
    return 0
}

# ============================================================================
# TEST WORKSPACE SETUP
# ============================================================================

setup_test_workspace() {
    local test_name="$1"
    local workspace_dir

    workspace_dir="$TEST_WORKSPACE_ROOT/$test_name-$$"

    # Create workspace directory
    mkdir -p "$workspace_dir"

    # Initialize git repo if not already
    if [[ ! -d "$workspace_dir/.git" ]]; then
        git init -q "$workspace_dir"
        git -C "$workspace_dir" config user.name "needle-routing-test"
        git -C "$workspace_dir" config user.email "needle-test@invalid"
        echo "# Routing test workspace: $test_name" > "$workspace_dir/README.md"
        git -C "$workspace_dir" add README.md
        git -C "$workspace_dir" commit -q -m "Initial commit for routing test" README.md
    fi

    # Initialize bead store if not already
    if [[ ! -d "$workspace_dir/.beads" ]]; then
        (cd "$workspace_dir" && bead init --prefix route >/dev/null 2>&1) || {
            log_error "Failed to initialize bead store in $workspace_dir"
            rm -rf "$workspace_dir"
            return 1
        }
    fi

    echo "$workspace_dir"
}

cleanup_test_workspace() {
    local workspace_dir="$1"

    if [[ -d "$workspace_dir" && "$workspace_dir" == "$TEST_WORKSPACE_ROOT"/* ]]; then
        log_debug "Cleaning up workspace: $workspace_dir"
        rm -rf "$workspace_dir"
    fi
}

# ============================================================================
# BEAD CREATION AND DISPATCH
# ============================================================================

dispatch_test_bead() {
    local test_name="$1"
    local model="$2"
    local expected_adapter="${3:-}"
    local workspace_dir="${4:-}"
    local priority="${5:-0}"

    log_info "Creating test bead for: $test_name"

    # Setup workspace if not provided
    if [[ -z "$workspace_dir" ]]; then
        workspace_dir=$(setup_test_workspace "$test_name") || return 1
    fi

    # Create bead with routing-specific metadata
    local bead_id
    bead_id=$(
        cd "$workspace_dir"
        bead create \
            --title "Routing Test: $test_name" \
            --priority "$priority" \
            --issue-type test \
            --label routing-test \
            --label "test-$test_name" \
            2>&1 | tail -1 | grep -oE '[a-z0-9-]+'
    ) || {
        log_error "Failed to create test bead"
        return 1
    }

    if [[ -z "$bead_id" ]]; then
        log_error "Empty bead ID returned from bead create"
        return 1
    fi

    # Add bead description with model request
    local description="Routing test bead for model: $model"
    if [[ -n "$expected_adapter" ]]; then
        description+="

Expected routing configuration:
- Model: $model
- Expected adapter: $expected_adapter

This bead should route through the expected adapter and complete successfully."
    fi

    (
        cd "$workspace_dir"
        bead update "$bead_id" --notes "$description" >/dev/null 2>&1 || true
    )

    log_info "✓ Created test bead: $bead_id"
    log_debug "  Workspace: $workspace_dir"
    log_debug "  Model: $model"
    log_debug "  Expected adapter: ${expected_adapter:-<auto-detect>}"

    # Return bead ID and workspace as space-separated values
    echo "$bead_id $workspace_dir"
}

# ============================================================================
# BEAD STATUS AND COMPLETION VERIFICATION
# ============================================================================

get_bead_status() {
    local workspace_dir="$1"
    local bead_id="$2"

    (
        cd "$workspace_dir"
        bead list --json --limit 1000 2>/dev/null | \
            jq -r --arg id "$bead_id" 'select(.id == $id) | .status'
    )
}

verify_bead_completion() {
    local workspace_dir="$1"
    local bead_id="$2"
    local expected_status="${3:-closed}"

    log_info "Verifying bead completion: $bead_id"

    local status
    status=$(get_bead_status "$workspace_dir" "$bead_id")

    if [[ -z "$status" ]]; then
        log_error "Bead not found: $bead_id"
        return 1
    fi

    if [[ "$status" == "$expected_status" ]]; then
        log_info "✓ Bead status: $status"
        return 0
    else
        log_warning "Bead status: $status (expected: $expected_status)"
        return 1
    fi
}

wait_for_bead_completion() {
    local workspace_dir="$1"
    local bead_id="$2"
    local timeout_secs="${3:-120}"
    local expected_status="${4:-closed}"

    log_info "Waiting for bead completion (timeout: ${timeout_secs}s)..."

    local elapsed=0
    local interval=2

    while [[ $elapsed -lt $timeout_secs ]]; do
        local status
        status=$(get_bead_status "$workspace_dir" "$bead_id")

        if [[ "$status" == "$expected_status" ]]; then
            log_info "✓ Bead reached status: $status"
            return 0
        fi

        log_debug "Bead status: $status (${elapsed}s elapsed)"
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done

    log_error "Timeout waiting for bead to reach status: $expected_status"
    log_error "Final status: $(get_bead_status "$workspace_dir" "$bead_id")"
    return 1
}

# ============================================================================
# TELEMETRY AND TRACE VERIFICATION
# ============================================================================

find_telemetry_file() {
    local workspace_dir="$1"
    local worker_name="$2"

    # Find telemetry file for the worker
    find "$workspace_dir/.beads" -type f \
        -name "$worker_name-*.jsonl" ! -name '*.agent.jsonl' \
        -print -quit 2>/dev/null
}

find_trace_file() {
    local workspace_dir="$1"
    local bead_id="$2"

    # Find trace file for the bead
    local trace_dir="$workspace_dir/.beads/traces/$bead_id"

    if [[ -d "$trace_dir" ]]; then
        if [[ -f "$trace_dir/trace.jsonl" ]]; then
            echo "$trace_dir/trace.jsonl"
        elif [[ -f "$trace_dir/stderr.txt" ]]; then
            echo "$trace_dir/stderr.txt"
        fi
    fi
}

verify_telemetry_event() {
    local workspace_dir="$1"
    local worker_name="$2"
    local bead_id="$3"
    local event_type="$4"
    local additional_filter="${5:-.}"

    log_info "Verifying telemetry event: $event_type"

    local telemetry_file
    telemetry_file=$(find_telemetry_file "$workspace_dir" "$worker_name")

    if [[ -z "$telemetry_file" ]]; then
        log_error "Telemetry file not found for worker: $worker_name"
        return 1
    fi

    log_debug "  Telemetry file: $telemetry_file"

    # Check for the event with optional additional filter
    local found
    found=$(jq -e \
        --arg bead "$bead_id" \
        --arg type "$event_type" \
        "select(.event_type == \$type and .bead_id == \$bead) | $additional_filter" \
        "$telemetry_file" 2>/dev/null | head -1)

    if [[ -n "$found" ]]; then
        log_info "✓ Found telemetry event: $event_type"
        log_debug "  Event data: $found"
        return 0
    else
        log_error "Telemetry event not found: $event_type"
        return 1
    fi
}

verify_routing_decision() {
    local workspace_dir="$1"
    local worker_name="$2"
    local bead_id="$3"
    local expected_model="$4"
    local expected_adapter="$5"

    log_info "Verifying routing decision"

    # Build filter to check routing decision
    local additional_filter
    additional_filter="select(
        .data.model == \"$expected_model\" and
        .data.chosen_adapter == \"$expected_adapter\"
    )"

    if verify_telemetry_event "$workspace_dir" "$worker_name" "$bead_id" \
        "agent.routing_decision" "$additional_filter"; then
        log_info "✓ Routing decision: $expected_model → $expected_adapter"
        return 0
    else
        log_error "Routing decision mismatch"
        log_error "  Expected: $expected_model → $expected_adapter"
        return 1
    fi
}

verify_agent_completion() {
    local workspace_dir="$1"
    local worker_name="$2"
    local bead_id="$3"
    local expected_agent="$4"
    local expected_exit_code="${5:-0}"

    log_info "Verifying agent completion"

    # Build filter to check agent completion
    local additional_filter
    additional_filter="select(
        .data.agent == \"$expected_agent\" and
        .data.exit_code == $expected_exit_code
    )"

    if verify_telemetry_event "$workspace_dir" "$worker_name" "$bead_id" \
        "agent.completed" "$additional_filter"; then
        log_info "✓ Agent completed: $expected_agent (exit: $expected_exit_code)"
        return 0
    else
        log_error "Agent completion verification failed"
        log_error "  Expected agent: $expected_agent"
        log_error "  Expected exit code: $expected_exit_code"
        return 1
    fi
}

# ============================================================================
# BINARY INVOCATION VERIFICATION
# ============================================================================

verify_invoked_binary() {
    local workspace_dir="$1"
    local bead_id="$2"
    local expected_binary="$3"

    log_info "Verifying binary invocation: $expected_binary"

    # Check trace file for binary invocation
    local trace_file
    trace_file=$(find_trace_file "$workspace_dir" "$bead_id")

    if [[ -z "$trace_file" ]]; then
        log_warning "Trace file not found for bead: $bead_id"
        return 1
    fi

    log_debug "  Trace file: $trace_file"

    # Look for the binary name in the trace
    if grep -q "$expected_binary" "$trace_file" 2>/dev/null; then
        log_info "✓ Binary invocation found: $expected_binary"
        return 0
    else
        log_error "Binary invocation not found: $expected_binary"
        return 1
    fi
}

verify_invocation_flags() {
    local workspace_dir="$1"
    local bead_id="$2"
    local expected_flags=("${@:3}")  # Array of expected flags

    log_info "Verifying invocation flags"

    local trace_file
    trace_file=$(find_trace_file "$workspace_dir" "$bead_id")

    if [[ -z "$trace_file" ]]; then
        log_warning "Trace file not found for bead: $bead_id"
        return 1
    fi

    local all_found=0
    for flag in "${expected_flags[@]}"; do
        if grep -q -- "$flag" "$trace_file" 2>/dev/null; then
            log_debug "  ✓ Found flag: $flag"
        else
            log_warning "  ✗ Missing flag: $flag"
            all_found=1
        fi
    done

    if [[ $all_found -eq 0 ]]; then
        log_info "✓ All expected flags present"
        return 0
    else
        log_warning "Some flags missing"
        return 1
    fi
}

# ============================================================================
# OUTPUT FORMAT VERIFICATION
# ============================================================================

verify_stream_json_output() {
    local workspace_dir="$1"
    local bead_id="$2"

    log_info "Verifying stream-json output format"

    # Check for stream-json in trace or separate output file
    local trace_file
    trace_file=$(find_trace_file "$workspace_dir" "$bead_id")

    if [[ -z "$trace_file" ]]; then
        log_warning "Trace file not found for bead: $bead_id"
        return 1
    fi

    # Look for stream-json markers
    local stream_count=0
    if [[ -f "$trace_file" ]]; then
        stream_count=$(grep -c "^data:" "$trace_file" 2>/dev/null || echo "0")
    fi

    if [[ "$stream_count" -gt 0 ]]; then
        log_info "✓ Found $stream_count stream-json data lines"
        return 0
    else
        log_warning "No stream-json data lines found"
        return 1
    fi
}

# ============================================================================
# BEAD CLEANUP
# ============================================================================

cleanup_test_bead() {
    local workspace_dir="$1"
    local bead_id="$2"
    local force="${3:-false}"

    log_info "Cleaning up test bead: $bead_id"

    local status
    status=$(get_bead_status "$workspace_dir" "$bead_id" 2>/dev/null || echo "")

    if [[ -z "$status" ]]; then
        log_warning "Bead not found, nothing to cleanup"
        return 0
    fi

    # Update bead status to deferred if not already closed
    if [[ "$status" != "closed" ]]; then
        (
            cd "$workspace_dir"
            bead update "$bead_id" --status deferred \
                --notes "Test cleanup: $(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
                >/dev/null 2>&1 || true
        )
        log_info "✓ Bead marked as deferred"
    else
        log_info "✓ Bead already closed"
    fi

    # Optionally force cleanup by closing the bead
    if [[ "$force" == "true" && "$status" != "closed" ]]; then
        (
            cd "$workspace_dir"
            bead close "$bead_id" \
                --reason "Force cleanup by routing test helpers" \
                >/dev/null 2>&1 || true
        )
        log_info "✓ Bead force-closed"
    fi
}

# ============================================================================
# WORKER EXECUTION HELPERS
# ============================================================================

run_worker_for_bead() {
    local workspace_dir="$1"
    local bead_id="$2"
    local model="$3"
    local timeout_secs="${4:-$TEST_TIMEOUT_SECS}"

    log_info "Running worker for bead: $bead_id (timeout: ${timeout_secs}s)"

    local output_log="/tmp/needle-worker-$bead_id-$$-log.txt"

    # Run needle run with timeout and correct parameters
    timeout "$timeout_secs" \
        needle run \
            --workspace "$workspace_dir" \
            --agent "$model" \
            --identifier "test-$bead_id" \
            --timeout 600 \
        2>&1 | tee "$output_log"

    local exit_code=$?

    if [[ $exit_code -eq 124 ]]; then
        log_error "Worker timed out after ${timeout_secs}s"
        return 1
    elif [[ $exit_code -ne 0 ]]; then
        log_error "Worker failed with exit code: $exit_code"
        log_error "  Log file: $output_log"
        return 1
    fi

    log_info "✓ Worker completed successfully"
    return 0
}

# ============================================================================
# TEST RESULT REPORTING
# ============================================================================

print_test_summary() {
    local test_name="$1"
    local all_passed="$2"
    local total_tests="$3"
    local passed_tests="$4"

    echo
    log_section "Test Summary: $test_name"
    echo
    echo "Total tests: $total_tests"
    echo "Passed: $passed_tests"
    echo "Failed: $((total_tests - passed_tests))"
    echo

    if [[ "$all_passed" -eq 0 ]]; then
        log_info "╔══════════════════════════════════════════════════════════════════╗"
        log_info "║  ✓ All tests PASSED                                           ║"
        log_info "╚══════════════════════════════════════════════════════════════════╝"
    else
        log_error "╔══════════════════════════════════════════════════════════════════╗"
        log_error "║  ✗ Some tests FAILED                                          ║"
        log_error "╚══════════════════════════════════════════════════════════════════╝"
    fi
    echo
}

# ============================================================================
# INITIALIZATION
# ============================================================================

# Auto-check prerequisites on source
if [[ "${ROUTING_TEST_AUTO_INIT:-1}" == "1" ]]; then
    if ! check_prerequisites 2>/dev/null; then
        log_warning "Routing test helpers loaded, but prerequisites check failed"
        log_warning "Some functions may not work correctly"
    fi
fi

log_debug "Routing test helpers loaded successfully"
