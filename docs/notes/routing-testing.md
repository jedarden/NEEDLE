# Routing Test Framework Documentation

This document explains how to use the routing test helper functions for NEEDLE model-based adapter routing verification.

## Overview

The routing test framework (`tests/routing-test-helpers.sh`) provides reusable shell functions for testing model-based adapter routing. It helps verify that:

- Models route through the correct adapters (e.g., `claude-sonnet-4-6` → `claude-print`)
- The correct binaries are invoked during dispatch
- Beads complete successfully with proper telemetry
- Output formats match expectations (e.g., `stream-json`)

## Setup

### Prerequisites

The helper script requires:
- `bead` CLI (bead-rs backend)
- `jq` for JSON parsing
- `needle` binary in PATH
- `git` for workspace initialization

### Sourcing the Helpers

Source the helpers at the beginning of your test script:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Source routing test helpers
source tests/routing-test-helpers.sh

# Now use helper functions in your tests
```

## Helper Functions

### Logging Functions

#### `log_info`, `log_error`, `log_warning`, `log_debug`

Colored logging functions for consistent output:

```bash
log_info "Starting test execution"
log_error "Test failed: missing binary"
log_warning "Telemetry file not found"
log_debug "Bead ID: $bead_id"  # Only shown when DEBUG=1
```

Enable debug output:
```bash
DEBUG=1 ./my_test.sh
```

#### `log_section`

Print a section header:
```bash
log_section "Test Phase 1: Bead Creation"
# Output: ==================== Test Phase 1: Bead Creation ====================
```

### Prerequisites

#### `check_prerequisites`

Verify required tools are available:

```bash
if ! check_prerequisites; then
    log_error "Missing required tools"
    exit 1
fi
```

### Workspace Management

#### `setup_test_workspace`

Create an isolated test workspace:

```bash
workspace_dir=$(setup_test_workspace "my-test")
echo "Created workspace: $workspace_dir"
```

The function:
- Creates a new directory under `/tmp/needle-routing-tests/`
- Initializes a git repository
- Sets up a bead store with `bead init`

#### `cleanup_test_workspace`

Clean up a test workspace:

```bash
cleanup_test_workspace "$workspace_dir"
```

**Safety:** Only removes directories matching the test root pattern to avoid accidental deletion.

### Bead Operations

#### `dispatch_test_bead`

Create a test bead with routing metadata:

```bash
result=$(dispatch_test_bead \
    "test-name" \
    "claude-sonnet-4-6" \
    "claude-print" \
    "$workspace_dir" \
    0)

bead_id=$(echo "$result" | cut -d' ' -f1)
workspace_dir=$(echo "$result" | cut -d' ' -f2)
```

Parameters:
1. `test_name` - Identifier for the test
2. `model` - Model to request (e.g., `claude-sonnet-4-6`)
3. `expected_adapter` - Expected adapter (optional, for documentation)
4. `workspace_dir` - Workspace directory (optional, auto-created if omitted)
5. `priority` - Bead priority (default: 0)

Returns: Space-separated `bead_id workspace_dir`

The function:
- Creates a bead with `--issue-type test` and routing labels
- Adds metadata documenting the expected routing behavior
- Returns both bead ID and workspace directory

#### `get_bead_status`

Get the current status of a bead:

```bash
status=$(get_bead_status "$workspace_dir" "$bead_id")
echo "Bead status: $status"
```

Returns: One of `open`, `in_progress`, `closed`, `deferred`

#### `verify_bead_completion`

Verify a bead reached the expected status:

```bash
verify_bead_completion "$workspace_dir" "$bead_id" "closed"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `expected_status` - Expected status (default: `closed`)

#### `wait_for_bead_completion`

Wait for a bead to reach a specific status:

```bash
wait_for_bead_completion "$workspace_dir" "$bead_id" 120 "closed"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `timeout_secs` - Maximum wait time (default: 120s)
4. `expected_status` - Expected status (default: `closed`)

### Telemetry and Trace Verification

#### `find_telemetry_file`

Locate the telemetry file for a worker:

```bash
telemetry_file=$(find_telemetry_file "$workspace_dir" "$worker_name")
```

#### `find_trace_file`

Locate the trace file for a bead:

```bash
trace_file=$(find_trace_file "$workspace_dir" "$bead_id")
```

#### `verify_telemetry_event`

Verify a specific telemetry event occurred:

```bash
verify_telemetry_event \
    "$workspace_dir" \
    "claude-print-worker-123" \
    "$bead_id" \
    "agent.started"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `worker_name` - Worker identifier
3. `bead_id` - Bead identifier
4. `event_type` - Event type to verify (e.g., `agent.started`)
5. `additional_filter` - Optional jq filter for event data

#### `verify_routing_decision`

Verify the routing decision telemetry event:

```bash
verify_routing_decision \
    "$workspace_dir" \
    "$worker_name" \
    "$bead_id" \
    "claude-sonnet-4-6" \
    "claude-print"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `worker_name` - Worker identifier
3. `bead_id` - Bead identifier
4. `expected_model` - Model that was requested
5. `expected_adapter` - Adapter that should have been chosen

This verifies that the `agent.routing_decision` event shows the correct model-to-adapter mapping.

#### `verify_agent_completion`

Verify an agent completed successfully:

```bash
verify_agent_completion \
    "$workspace_dir" \
    "$worker_name" \
    "$bead_id" \
    "claude-print" \
    0
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `worker_name` - Worker identifier
3. `bead_id` - Bead identifier
4. `expected_agent` - Expected agent/adapter name
5. `expected_exit_code` - Expected exit code (default: 0)

### Binary Invocation Verification

#### `verify_invoked_binary`

Verify the expected binary was invoked:

```bash
verify_invoked_binary "$workspace_dir" "$bead_id" "claude-print"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `expected_binary` - Binary name to look for

This searches trace files for evidence that the specified binary was executed.

#### `verify_invocation_flags`

Verify specific flags were passed to the binary:

```bash
verify_invocation_flags "$workspace_dir" "$bead_id" \
    "--stream-json" "--model" "claude-sonnet-4-6"
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `expected_flags` - Array of expected flags

### Output Format Verification

#### `verify_stream_json_output`

Verify output contains stream-json format:

```bash
verify_stream_json_output "$workspace_dir" "$bead_id"
```

This looks for `^data:` lines in trace files, which indicate stream-json output format.

### Worker Execution

#### `run_worker_for_bead`

Run a NEEDLE worker to process a specific bead:

```bash
run_worker_for_bead "$workspace_dir" "$bead_id" "claude-sonnet-4-6" 600
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `model` - Model to use for dispatch
4. `timeout_secs` - Worker timeout (default: `TEST_TIMEOUT_SECS` or 600s)

The function:
- Runs `needle worker --workspace --once --bead --model`
- Streams output to a log file
- Returns failure if worker times out or exits non-zero

### Cleanup

#### `cleanup_test_bead`

Clean up a test bead after testing:

```bash
cleanup_test_bead "$workspace_dir" "$bead_id"
cleanup_test_bead "$workspace_dir" "$bead_id" true  # Force close
```

Parameters:
1. `workspace_dir` - Workspace directory
2. `bead_id` - Bead identifier
3. `force` - If `true`, force close the bead (default: `false`)

The function:
- Marks the bead as `deferred` if not already closed
- Optionally force-closes the bead

### Result Reporting

#### `print_test_summary`

Print a formatted test summary:

```bash
print_test_summary "My Test" $all_passed $total_tests $passed_tests
```

Parameters:
1. `test_name` - Test identifier
2. `all_passed` - Exit code (0 for all passed, 1 for any failure)
3. `total_tests` - Total number of tests run
4. `passed_tests` - Number of tests passed

## Example Usage

### Complete Test Example

```bash
#!/usr/bin/env bash
set -euo pipefail

source tests/routing-test-helpers.sh

main() {
    local test_name="anthropic-sonnet-routing"
    local workspace_dir
    local bead_id
    local worker_name="test-worker-$$"
    local all_passed=0
    local total_tests=0
    local passed_tests=0

    log_section "Test: $test_name"

    # Setup
    if ! check_prerequisites; then
        log_error "Prerequisites check failed"
        exit 1
    fi

    ((total_tests++)) || true
    workspace_dir=$(setup_test_workspace "$test_name") || {
        log_error "Workspace setup failed"
        ((all_passed++)) || true
    }
    ((passed_tests++)) || true

    # Create bead
    ((total_tests++)) || true
    result=$(dispatch_test_bead \
        "$test_name" \
        "claude-sonnet-4-6" \
        "claude-print" \
        "$workspace_dir") || {
        log_error "Bead creation failed"
        ((all_passed++)) || true
        exit 1
    }
    ((passed_tests++)) || true

    bead_id=$(echo "$result" | cut -d' ' -f1)

    # Run worker
    ((total_tests++)) || true
    if ! run_worker_for_bead "$workspace_dir" "$bead_id" "claude-sonnet-4-6"; then
        log_error "Worker execution failed"
        ((all_passed++)) || true
    fi
    ((passed_tests++)) || true

    # Verify routing decision
    ((total_tests++)) || true
    if ! verify_routing_decision "$workspace_dir" "$worker_name" "$bead_id" \
        "claude-sonnet-4-6" "claude-print"; then
        log_error "Routing decision verification failed"
        ((all_passed++)) || true
    fi
    ((passed_tests++)) || true

    # Verify bead completion
    ((total_tests++)) || true
    if ! verify_bead_completion "$workspace_dir" "$bead_id" "closed"; then
        log_error "Bead completion verification failed"
        ((all_passed++)) || true
    fi
    ((passed_tests++)) || true

    # Cleanup
    cleanup_test_bead "$workspace_dir" "$bead_id"
    cleanup_test_workspace "$workspace_dir"

    # Report
    print_test_summary "$test_name" $all_passed $total_tests $passed_tests
    exit $all_passed
}

main "$@"
```

## Environment Variables

Configure helper behavior with these environment variables:

- `DEBUG` - Set to `1` to enable debug logging
- `TEST_WORKSPACE_ROOT` - Root directory for test workspaces (default: `/tmp/needle-routing-tests`)
- `TEST_TIMEOUT_SECS` - Default timeout for worker execution (default: `600`)
- `ROUTING_TEST_AUTO_INIT` - Set to `0` to skip automatic prerequisite check on source (default: `1`)

## Integration with Existing Tests

The routing test helpers integrate with existing NEEDLE test infrastructure:

### Rust Integration Tests

Use helpers in shell scripts that complement Rust tests:

```bash
# Run Rust unit tests first
cargo test routing

# Then run E2E verification with helpers
./tests/my_routing_e2e_test.sh
```

### CI/CD Integration

The helpers work in the `needle-ci` Argo Workflow:

```yaml
# Example Argo Workflow step
- name: routing-verification
  script: |
    #!/bin/bash
    set -euo pipefail
    source tests/routing-test-helpers.sh
    # Run test scenarios
```

## Best Practices

1. **Always clean up** - Use `cleanup_test_bead` and `cleanup_test_workspace` in EXIT traps
2. **Set timeouts** - Use `wait_for_bead_completion` to avoid hanging tests
3. **Check prerequisites** - Call `check_prerequisites` at the start of each test
4. **Use workspace isolation** - Each test should have its own workspace
5. **Verify telemetry** - Always check routing decision events, not just output
6. **Debug output** - Use `DEBUG=1` when troubleshooting test failures

## Troubleshooting

### "Telemetry file not found"

- Verify the worker actually ran
- Check the worker name matches expectations
- Ensure sufficient time for worker to write telemetry

### "Bead creation failed"

- Verify bead CLI is available
- Check workspace is properly initialized
- Ensure bead store exists (`bead init`)

### "Routing decision mismatch"

- Verify routing rules in `.needle.yaml`
- Check model name matches exactly
- Confirm adapter is configured in `adapters_dir`

### "Worker timed out"

- Increase `TEST_TIMEOUT_SECS`
- Check if model/adapter combination is valid
- Verify network connectivity for external models

## Routing-Decision Telemetry Verification

Both routing test scripts verify that routing-decision telemetry events are properly emitted during bead processing. This provides end-to-end validation that the routing system is working correctly.

### Event Structure

The `agent.routing_decision` telemetry event contains:

```json
{
  "event_type": "agent.routing_decision",
  "bead_id": "<bead-id>",
  "timestamp": "<ISO-8601-timestamp>",
  "data": {
    "model": "<requested-model>",
    "chosen_adapter": "<adapter-name>",
    "decision_reason": "<explanation>"
  }
}
```

### Verification in Tests

#### claude-print Routing Test

The `tests/routing-claude-print.sh` test verifies routing telemetry for Anthropic subscription models:

```bash
# Expected routing: claude-sonnet-4-6 → claude-print
verify_claude_print_invocation_in_telemetry \
    "$workspace_dir" \
    "$worker_name" \
    "$bead_id"
```

This checks:
- Telemetry file exists for the worker
- `agent.routing_decision` event is present
- Event's `chosen_adapter` field matches `claude-print`

#### glm-4.7 Routing Test

The `tests/routing-glm-4.7.sh` test verifies routing telemetry for glm-4.7 model:

```bash
# Expected routing: glm-4.7 → claude-code-glm-4.7 (default adapter)
# Extract routing event from telemetry
routing_event=$(jq -r --arg bead "$bead_id" \
    'select(.bead_id == $bead and .event_type == "agent.routing_decision")' \
    "$telemetry_file" | head -1)

# Verify event structure
event_model=$(echo "$routing_event" | jq -r '.data.model')
event_adapter=$(echo "$routing_event" | jq -r '.data.chosen_adapter')
```

This checks:
- Routing decision event is present
- Event contains both `model` and `chosen_adapter` fields
- `chosen_adapter` matches expected `claude-code-glm-4.7`

### Using the Helper Function

For new routing tests, use the `verify_routing_decision()` helper from `routing-test-helpers.sh`:

```bash
verify_routing_decision \
    "$workspace_dir" \
    "$worker_name" \
    "$bead_id" \
    "expected-model" \
    "expected-adapter"
```

This validates the complete routing decision event structure.

## See Also

- `tests/routing-claude-print.sh` - Anthropic subscription model routing test
- `tests/routing-glm-4.7.sh` - glm-4.7 model routing test
- `tests/routing-test-helpers.sh` - Helper functions for routing tests
- `docs/notes/routing-test-results.md` - Sample test results
- NEEDLE ADR-XXX: Routing Design Decisions
