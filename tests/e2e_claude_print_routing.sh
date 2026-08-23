#!/usr/bin/env bash
# End-to-end integration test for claude-print routing (bf-2xi)
#
# This test validates model-based adapter routing on this host:
# 1. Anthropic subscription models (sonnet, opus, fable, haiku) → claude-print
# 2. GLM models → claude-code-glm-4.7 (default adapter)
# 3. Routing-decision telemetry events are emitted
# 4. Missing adapter causes loud failure (no silent fallback)
#
# Usage: ./tests/e2e_claude_print_routing.sh
#
# Acceptance: All four scenarios pass

set -euo pipefail

# ═══════════════════════════════════════════════════════════════════════════════
# Test Configuration
# ═══════════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
NEEDLE_BIN="$PROJECT_ROOT/target/debug/needle"

# Test workspace (temporary, isolated)
TEST_WORKSPACE="$(mktemp -d)"
TEST_REGISTRY="$(mktemp -d)"

# Adapter binaries
CLAUDE_PRINT_BIN="$(which claude-print)"
CLAUDE_CODE_GLM_BIN="$(which claude-code-glm-4.7)"

# Telemetry output
TELEMETRY_DIR="$(mktemp -d)"

# Cleanup trap
cleanup() {
    local exit_code=$?

    echo "Cleaning up test artifacts..."

    # Restore claude-print if it was renamed
    if [[ -f "${CLAUDE_PRINT_BIN}.backup" ]]; then
        mv "${CLAUDE_PRINT_BIN}.backup" "$CLAUDE_PRINT_BIN"
        echo "✓ Restored claude-print binary"
    fi

    # Clean up test directories
    rm -rf "$TEST_WORKSPACE" "$TEST_REGISTRY" "$TELEMETRY_DIR"

    echo "Cleanup complete (exit code: $exit_code)"
    exit $exit_code
}

trap cleanup EXIT INT TERM

# ═══════════════════════════════════════════════════════════════════════════════
# Test Helpers
# ═══════════════════════════════════════════════════════════════════════════════

log_info() {
    echo "[INFO] $*"
}

log_success() {
    echo "[✓] $*"
}

log_error() {
    echo "[✗] $*"
}

test_fail() {
    log_error "$1"
    exit 1
}

# Build needle if needed
build_needle() {
    log_info "Building needle binary..."
    if [[ ! -f "$NEEDLE_BIN" || "$PROJECT_ROOT/src" -nt "$NEEDLE_BIN" ]]; then
        cargo build --quiet 2>&1 | grep -q "Finished" || {
            test_fail "Failed to build needle binary"
        }
        log_success "Built needle binary: $NEEDLE_BIN"
    else
        log_info "needle binary already up-to-date"
    fi
}

# Verify adapter binaries exist
verify_adapters() {
    log_info "Verifying adapter binaries..."

    if [[ ! -x "$CLAUDE_PRINT_BIN" ]]; then
        test_fail "claude-print not found or not executable: $CLAUDE_PRINT_BIN"
    fi
    log_success "Found claude-print: $CLAUDE_PRINT_BIN"

    if [[ ! -x "$CLAUDE_CODE_GLM_BIN" ]]; then
        test_fail "claude-code-glm-4.7 not found or not executable: $CLAUDE_CODE_GLM_BIN"
    fi
    log_success "Found claude-code-glm-4.7: $CLAUDE_CODE_GLM_BIN"
}

# Initialize a test bead workspace
init_test_workspace() {
    local workspace="$1"

    log_info "Initializing test workspace: $workspace"

    mkdir -p "$workspace"

    # Initialize bead store (bead-rs backend)
    cd "$workspace"

    if bead init 2>&1 | grep -q "Initialized"; then
        log_success "Initialized bead workspace"
    else
        # May already be initialized
        log_info "Workspace may already be initialized"
    fi
}

# Create a trivial test bead
create_test_bead() {
    local title="$1"
    local model="$2"

    log_info "Creating test bead: $title (model: $model)"

    # Create bead with model-specific instructions
    bead create \
        --title "$title" \
        --priority 0 \
        --issue-type task \
        --label "test-routing" \
        --notes "Test bead for routing validation. Requested model: $model" \
        2>&1 | grep -E "Created|created" || {
        test_fail "Failed to create bead"
    }

    # Get the bead ID from the output
    bead list --json --limit 1 | jq -r '.[0].id' | head -1
}

# Process a bead with needle worker and capture output
process_bead_with_needle() {
    local bead_id="$1"
    local model="$2"
    local output_file="$3"

    log_info "Processing bead $bead_id with model: $model"

    # Run needle worker with telemetry capture
    HOME="$TEST_WORKSPACE" \
    NEEDLE_TELEMETRY_FILE="$output_file" \
    "$NEEDLE_BIN" worker \
        --once \
        --adapter="$model" \
        --model="$model" \
        --workspace="$TEST_WORKSPACE" \
        --registry="$TEST_REGISTRY" \
        2>&1 || true

    log_success "Worker completed for bead $bead_id"
}

# Verify routing decision in telemetry
verify_routing_telemetry() {
    local telemetry_file="$1"
    local expected_adapter="$2"

    log_info "Verifying routing telemetry..."

    if [[ ! -f "$telemetry_file" ]]; then
        test_fail "Telemetry file not found: $telemetry_file"
    fi

    # Check for routing_decision event
    if grep -q "routing_decision" "$telemetry_file"; then
        log_success "Found routing_decision event"
    else
        test_fail "No routing_decision event found in telemetry"
    fi

    # Verify the adapter matches expected
    if grep -q "\"adapter\":\"$expected_adapter\"" "$telemetry_file"; then
        log_success "Routing decision correct: $expected_adapter"
    else
        log_error "Expected adapter '$expected_adapter' not found in telemetry"
        echo "Telemetry content:"
        cat "$telemetry_file" | head -20
        test_fail "Routing decision mismatch"
    fi
}

# ═══════════════════════════════════════════════════════════════════════════════
# Test Scenarios
# ═══════════════════════════════════════════════════════════════════════════════

# Scenario 1: Anthropic Sonnet → claude-print
test_scenario_1_anthropic_sonnet() {
    log_info "=== Scenario 1: Anthropic Sonnet → claude-print ==="

    init_test_workspace "$TEST_WORKSPACE/scenario1"

    # Create bead with sonnet model request
    local bead_id
    bead_id=$(create_test_bead "test-sonnet-routing" "claude-sonnet-4-7")

    # Process bead
    local telemetry_file="$TELEMETRY_DIR/scenario1.jsonl"
    process_bead_with_needle "$bead_id" "claude-sonnet-4-7" "$telemetry_file"

    # Verify routing
    verify_routing_telemetry "$telemetry_file" "claude-print"

    # Verify claude-print was actually invoked (check for invoke event)
    if grep -q "claude-print" "$telemetry_file"; then
        log_success "claude-print adapter invocation verified"
    else
        log_error "claude-print not invoked"
        cat "$telemetry_file" | head -10
        test_fail "claude-print invocation failed"
    fi

    log_success "Scenario 1 PASSED: Anthropic Sonnet routes to claude-print"
}

# Scenario 2: GLM-4.7 → claude-code-glm-4.7 (negative control)
test_scenario_2_glm47_routing() {
    log_info "=== Scenario 2: GLM-4.7 → claude-code-glm-4.7 ==="

    init_test_workspace "$TEST_WORKSPACE/scenario2"

    # Create bead with glm-4.7 model request
    local bead_id
    bead_id=$(create_test_bead "test-glm47-routing" "glm-4.7")

    # Process bead
    local telemetry_file="$TELEMETRY_DIR/scenario2.jsonl"
    process_bead_with_needle "$bead_id" "glm-4.7" "$telemetry_file"

    # Verify routing
    verify_routing_telemetry "$telemetry_file" "claude-code-glm-4.7"

    # Verify claude-code-glm-4.7 was actually invoked
    if grep -q "claude-code-glm-4.7" "$telemetry_file"; then
        log_success "claude-code-glm-4.7 adapter invocation verified"
    else
        log_error "claude-code-glm-4.7 not invoked"
        cat "$telemetry_file" | head -10
        test_fail "claude-code-glm-4.7 invocation failed"
    fi

    log_success "Scenario 2 PASSED: GLM-4.7 routes to claude-code-glm-4.7"
}

# Scenario 3: Verify routing telemetry events are emitted
test_scenario_3_routing_events() {
    log_info "=== Scenario 3: Routing telemetry events ==="

    init_test_workspace "$TEST_WORKSPACE/scenario3"

    # Create bead with explicit model
    local bead_id
    bead_id=$(create_test_bead "test-telemetry-events" "claude-opus-4-7")

    # Process bead
    local telemetry_file="$TELEMETRY_DIR/scenario3.jsonl"
    process_bead_with_needle "$bead_id" "claude-opus-4-7" "$telemetry_file"

    # Verify multiple telemetry events are present
    local required_events=(
        "routing_decision"
        "worker_start"
        "worker_claim"
        "worker_dispatch"
    )

    for event in "${required_events[@]}"; do
        if grep -q "\"event\":\"$event\"" "$telemetry_file" || \
           grep -q "\"$event\"" "$telemetry_file"; then
            log_success "Found telemetry event: $event"
        else
            log_error "Missing telemetry event: $event"
            echo "Telemetry content:"
            cat "$telemetry_file" | jq -c '.' 2>/dev/null || cat "$telemetry_file"
            test_fail "Telemetry event '$event' not found"
        fi
    done

    log_success "Scenario 3 PASSED: Routing telemetry events emitted"
}

# Scenario 4: Missing adapter failure (no silent fallback)
test_scenario_4_missing_adapter() {
    log_info "=== Scenario 4: Missing adapter loud failure ==="

    init_test_workspace "$TEST_WORKSPACE/scenario4"

    # Backup claude-print
    mv "$CLAUDE_PRINT_BIN" "${CLAUDE_PRINT_BIN}.backup"
    log_info "Temporarily renamed claude-print → claude-print.backup"

    # Create bead with sonnet model (should fail without claude-print)
    local bead_id
    bead_id=$(create_test_bead "test-missing-adapter" "claude-sonnet-4-7")

    # Process bead - should fail loudly
    local telemetry_file="$TELEMETRY_DIR/scenario4.jsonl"

    HOME="$TEST_WORKSPACE" \
    NEEDLE_TELEMETRY_FILE="$telemetry_file" \
    "$NEEDLE_BIN" worker \
        --once \
        --adapter="claude-sonnet-4-7" \
        --model="claude-sonnet-4-7" \
        --workspace="$TEST_WORKSPACE/scenario4" \
        --registry="$TEST_REGISTRY" \
        2>&1 | tee "$TELEMETRY_DIR/scenario4_output.log"

    local exit_code=${PIPESTATUS[0]}

    # Verify loud failure (non-zero exit, clear error message)
    if [[ $exit_code -ne 0 ]]; then
        log_success "Worker exited with non-zero code (expected failure)"
    else
        log_error "Worker should have failed but exited successfully"
        test_fail "Missing adapter did not cause failure"
    fi

    # Verify error message contains clear indication
    if grep -qi "adapter.*not found\|claude-print.*not found\|cannot.*find.*adapter" \
       "$TELEMETRY_DIR/scenario4_output.log"; then
        log_success "Clear error message about missing adapter"
    else
        log_error "Error message unclear about missing adapter"
        cat "$TELEMETRY_DIR/scenario4_output.log"
        test_fail "Missing adapter error message unclear"
    fi

    # Verify no silent fallback occurred
    if grep -q "claude-code-glm-4.7" "$TELEMETRY_DIR/scenario4_output.log" && \
       ! grep -q "claude-print" "$TELEMETRY_DIR/scenario4_output.log"; then
        log_error "Silent fallback to default adapter detected"
        test_fail "Silent fallback occurred (should fail loudly)"
    fi

    # Restore claude-print immediately
    mv "${CLAUDE_PRINT_BIN}.backup" "$CLAUDE_PRINT_BIN"
    log_success "Restored claude-print binary"

    log_success "Scenario 4 PASSED: Missing adapter fails loudly"
}

# ═══════════════════════════════════════════════════════════════════════════════
# Main Test Runner
# ═══════════════════════════════════════════════════════════════════════════════

main() {
    echo "═══════════════════════════════════════════════════════════════════════════"
    echo "End-to-End claude-print Routing Test"
    echo "═══════════════════════════════════════════════════════════════════════════"
    echo ""

    # Pre-flight checks
    build_needle
    verify_adapters

    # Check for required tools
    for tool in bead jq; do
        if ! command -v "$tool" &>/dev/null; then
            test_fail "Required tool not found: $tool"
        fi
    done
    log_success "All required tools available"

    echo ""
    echo "Running test scenarios..."
    echo ""

    # Run all scenarios
    test_scenario_1_anthropic_sonnet
    echo ""

    test_scenario_2_glm47_routing
    echo ""

    test_scenario_3_routing_events
    echo ""

    test_scenario_4_missing_adapter
    echo ""

    echo "═══════════════════════════════════════════════════════════════════════════"
    echo "All scenarios PASSED ✓"
    echo "═══════════════════════════════════════════════════════════════════════════"
}

# Run main
main "$@"
