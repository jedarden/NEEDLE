#!/usr/bin/env bash
# Test script for hooks/runner.sh module

set -euo pipefail

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Set up test environment
export NEEDLE_HOME="${TMPDIR:-/tmp}/needle-hooks-test-$$"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_LOG_INITIALIZED=false
mkdir -p "$NEEDLE_HOME/hooks"

# Source required modules
source "$NEEDLE_ROOT/src/lib/constants.sh"
source "$NEEDLE_ROOT/src/lib/output.sh"
source "$NEEDLE_ROOT/src/lib/paths.sh"
source "$NEEDLE_ROOT/src/lib/json.sh"
source "$NEEDLE_ROOT/src/lib/config.sh"
source "$NEEDLE_ROOT/src/lib/utils.sh"
source "$NEEDLE_ROOT/src/telemetry/events.sh"
source "$NEEDLE_ROOT/src/hooks/runner.sh"

# Initialize output system
export NEEDLE_VERBOSE="${NEEDLE_VERBOSE:-false}"
export NEEDLE_QUIET="${NEEDLE_QUIET:-false}"
_needle_output_init

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# Test helper functions
test_pass() {
    TESTS_PASSED=$((TESTS_PASSED + 1))
    echo "✓ $1"
}

test_fail() {
    TESTS_FAILED=$((TESTS_FAILED + 1))
    echo "✗ $1"
}

# Create hook script
create_hook() {
    local name="$1"
    local code="$2"
    cat > "$NEEDLE_HOME/hooks/$name" << HOOKCODE
#!/bin/bash
$code
HOOKCODE
    chmod +x "$NEEDLE_HOME/hooks/$name"
}

# Write config with proper variable expansion
write_hook_config() {
    local hook_type="$1"
    local hook_path="$2"
    local timeout="${3:-5s}"
    local fail_action="${4:-warn}"

    cat > "$NEEDLE_CONFIG_FILE" << CONFIGEOF
hooks:
  timeout: $timeout
  fail_action: $fail_action
  $hook_type: $hook_path
CONFIGEOF
    clear_config_cache
}

# Run tests
echo "=== Hook Runner Tests ==="
echo ""

# Test 1: No hook configured
echo "Test 1: No hook configured"
write_hook_config "on_quarantine" "/nonexistent/path.sh"
if _needle_run_hook "on_quarantine" "test-1" 2>/dev/null; then
    test_pass "Returns success when no hook configured"
else
    test_fail "Should return success when no hook configured"
fi

# Test 2: Hook file not found
echo ""
echo "Test 2: Hook file not found"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/missing.sh"
if _needle_run_hook "pre_claim" "test-2" 2>/dev/null; then
    test_pass "Returns success when hook file not found"
else
    test_fail "Should return success when hook file not found"
fi

# Test 3: Success hook (exit 0)
echo ""
echo "Test 3: Success hook (exit 0)"
create_hook "success.sh" "exit 0"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/success.sh"
if _needle_run_hook "pre_claim" "test-3" 2>/dev/null; then
    test_pass "Returns success for exit 0"
else
    test_fail "Should return success for exit 0"
fi

# Test 4: Warning hook (exit 1)
echo ""
echo "Test 4: Warning hook (exit 1)"
create_hook "warning.sh" "exit 1"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/warning.sh"
if _needle_run_hook "pre_claim" "test-4" 2>/dev/null; then
    test_pass "Returns success for exit 1 (warning)"
else
    test_fail "Should return success for exit 1 (warning)"
fi

# Test 5: Abort hook (exit 2)
echo ""
echo "Test 5: Abort hook (exit 2)"
create_hook "abort.sh" "exit 2"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/abort.sh"
if ! _needle_run_hook "pre_claim" "test-5" 2>/dev/null; then
    test_pass "Returns failure for exit 2 (abort)"
else
    test_fail "Should return failure for exit 2 (abort)"
fi

# Test 6: Skip hook (exit 3)
echo ""
echo "Test 6: Skip hook (exit 3)"
create_hook "skip.sh" "exit 3"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/skip.sh"
result=$(_needle_run_hook "pre_claim" "test-6" 2>/dev/null; echo $?)
if [[ "$result" == "2" ]]; then
    test_pass "Returns 2 for exit 3 (skip)"
else
    test_fail "Should return 2 for exit 3 (skip), got: $result"
fi

# Test 7: Timeout handling (fail_action=warn)
echo ""
echo "Test 7: Timeout handling (fail_action=warn)"
create_hook "slow.sh" "sleep 10
exit 0"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/slow.sh" "1s" "warn"
if _needle_run_hook "pre_claim" "test-7" 2>/dev/null; then
    test_pass "Returns success for timeout with fail_action=warn"
else
    test_fail "Should return success for timeout with fail_action=warn"
fi

# Test 8: Timeout with abort action
echo ""
echo "Test 8: Timeout handling (fail_action=abort)"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/slow.sh" "1s" "abort"
if ! _needle_run_hook "pre_claim" "test-8" 2>/dev/null; then
    test_pass "Returns failure for timeout with fail_action=abort"
else
    test_fail "Should return failure for timeout with fail_action=abort"
fi

# Test 9: Environment variables
echo ""
echo "Test 9: Environment variables are set"
cat > "$NEEDLE_HOME/hooks/env-check.sh" << 'ENVCHECK'
#!/bin/bash
[[ -n "$NEEDLE_HOOK" ]] || exit 1
[[ "$NEEDLE_BEAD_ID" == "test-9" ]] || exit 1
exit 0
ENVCHECK
chmod +x "$NEEDLE_HOME/hooks/env-check.sh"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/env-check.sh"
if _needle_run_hook "pre_claim" "test-9" 2>/dev/null; then
    test_pass "Environment variables are correctly set"
else
    test_fail "Environment variables not correctly set"
fi

# Test 10: List hooks
echo ""
echo "Test 10: List configured hooks"
echo "hooks:" > "$NEEDLE_CONFIG_FILE"
echo "  timeout: 5s" >> "$NEEDLE_CONFIG_FILE"
echo "  fail_action: warn" >> "$NEEDLE_CONFIG_FILE"
echo "  pre_claim: $NEEDLE_HOME/hooks/success.sh" >> "$NEEDLE_CONFIG_FILE"
echo "  post_claim: $NEEDLE_HOME/hooks/warning.sh" >> "$NEEDLE_CONFIG_FILE"
clear_config_cache
hooks_json=$(_needle_list_hooks)
if echo "$hooks_json" | grep -q "pre_claim" && echo "$hooks_json" | grep -q "post_claim"; then
    test_pass "List hooks returns configured hooks"
else
    test_fail "List hooks should return configured hooks"
fi

# Test 11: Validate hooks (pass)
echo ""
echo "Test 11: Validate hooks"
write_hook_config "pre_claim" "$NEEDLE_HOME/hooks/success.sh"
if _needle_validate_hooks 2>/dev/null; then
    test_pass "Validate hooks passes for existing hooks"
else
    test_fail "Validate hooks should pass for existing hooks"
fi

# Test 12: Convenience functions
echo ""
echo "Test 12: Convenience functions"
write_hook_config "pre_execute" "$NEEDLE_HOME/hooks/success.sh"
if _needle_hook_pre_execute "test-12" 2>/dev/null; then
    test_pass "Convenience function _needle_hook_pre_execute works"
else
    test_fail "Convenience function _needle_hook_pre_execute should work"
fi

# Test 13: Create sample hook
echo ""
echo "Test 13: Create sample hook"
sample_hook_path="$NEEDLE_HOME/hooks/sample-test.sh"
if _needle_create_sample_hook "pre_claim" "$sample_hook_path" 2>/dev/null; then
    if [[ -f "$sample_hook_path" ]] && [[ -x "$sample_hook_path" ]]; then
        test_pass "Create sample hook creates executable file"
    else
        test_fail "Sample hook file should be executable"
    fi
else
    test_fail "Create sample hook should succeed"
fi

# Cleanup
rm -rf "$NEEDLE_HOME"

# Print results
echo ""
echo "=== Test Results ==="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo ""

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "All tests passed!"
    exit 0
else
    echo "Some tests failed"
    exit 1
fi
