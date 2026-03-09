#!/usr/bin/env bash
# Test script for hooks/validate.sh module

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Set up test environment
export NEEDLE_HOME="${TMPDIR:-/tmp}/needle-validate-test-$$"
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
source "$NEEDLE_ROOT/src/hooks/validate.sh"

export NEEDLE_VERBOSE="${NEEDLE_VERBOSE:-false}"
export NEEDLE_QUIET="${NEEDLE_QUIET:-true}"
_needle_output_init

TESTS_PASSED=0
TESTS_FAILED=0

test_pass() { TESTS_PASSED=$((TESTS_PASSED + 1)); echo "✓ $1"; }
test_fail() { TESTS_FAILED=$((TESTS_FAILED + 1)); echo "✗ $1"; }

make_hook() {
    local name="$1"
    local body="$2"
    local path="$NEEDLE_HOME/hooks/$name"
    printf '#!/bin/bash\n%s\n' "$body" > "$path"
    chmod +x "$path"
    echo "$path"
}

write_cfg() {
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

echo "=== Hook Validate Tests ==="
echo ""

# ---------------------------------------------------------------------------
# _needle_validate_hook_executable
# ---------------------------------------------------------------------------

echo "Test 1: executable validation - missing file"
if ! _needle_validate_hook_executable "/nonexistent/hook.sh" 2>/dev/null; then
    test_pass "Returns failure for missing file"
else
    test_fail "Should fail for missing file"
fi

echo ""
echo "Test 2: executable validation - not executable"
NOEXEC="$NEEDLE_HOME/hooks/noexec.sh"
printf '#!/bin/bash\nexit 0\n' > "$NOEXEC"
chmod -x "$NOEXEC"
if ! _needle_validate_hook_executable "$NOEXEC" 2>/dev/null; then
    test_pass "Returns failure for non-executable file"
else
    test_fail "Should fail for non-executable file"
fi

echo ""
echo "Test 3: executable validation - valid file"
EXEC_HOOK=$(make_hook "exec.sh" "exit 0")
if _needle_validate_hook_executable "$EXEC_HOOK" 2>/dev/null; then
    test_pass "Returns success for executable file"
else
    test_fail "Should succeed for executable file"
fi

# ---------------------------------------------------------------------------
# _needle_validate_hook_syntax
# ---------------------------------------------------------------------------

echo ""
echo "Test 4: syntax check - valid script"
VALID_HOOK=$(make_hook "valid.sh" "echo hello\nexit 0")
if _needle_validate_hook_syntax "$VALID_HOOK" 2>/dev/null; then
    test_pass "Returns success for valid syntax"
else
    test_fail "Should succeed for valid syntax"
fi

echo ""
echo "Test 5: syntax check - invalid bash"
INVALID_HOOK="$NEEDLE_HOME/hooks/invalid.sh"
printf '#!/bin/bash\nif [ then\nexit 0\n' > "$INVALID_HOOK"
chmod +x "$INVALID_HOOK"
if ! _needle_validate_hook_syntax "$INVALID_HOOK" 2>/dev/null; then
    test_pass "Returns failure for syntax errors"
else
    test_fail "Should fail for syntax errors"
fi

echo ""
echo "Test 6: syntax check - missing file"
if ! _needle_validate_hook_syntax "/nonexistent/hook.sh" 2>/dev/null; then
    test_pass "Returns failure for missing file"
else
    test_fail "Should fail for missing file"
fi

# ---------------------------------------------------------------------------
# _needle_validate_hook_exit_code
# ---------------------------------------------------------------------------

echo ""
echo "Test 7: exit code 0 (success) is recognized"
if _needle_validate_hook_exit_code 0 2>/dev/null; then
    test_pass "Exit code 0 recognized"
else
    test_fail "Exit code 0 should be recognized"
fi

echo ""
echo "Test 8: exit code 1 (warning) is recognized"
if _needle_validate_hook_exit_code 1 2>/dev/null; then
    test_pass "Exit code 1 recognized"
else
    test_fail "Exit code 1 should be recognized"
fi

echo ""
echo "Test 9: exit code 2 (abort) is recognized"
if _needle_validate_hook_exit_code 2 2>/dev/null; then
    test_pass "Exit code 2 recognized"
else
    test_fail "Exit code 2 should be recognized"
fi

echo ""
echo "Test 10: exit code 3 (skip) is recognized"
if _needle_validate_hook_exit_code 3 2>/dev/null; then
    test_pass "Exit code 3 recognized"
else
    test_fail "Exit code 3 should be recognized"
fi

echo ""
echo "Test 11: exit code 124 (timeout) is recognized"
if _needle_validate_hook_exit_code 124 2>/dev/null; then
    test_pass "Exit code 124 recognized"
else
    test_fail "Exit code 124 should be recognized"
fi

echo ""
echo "Test 12: unknown exit code is rejected"
if ! _needle_validate_hook_exit_code 99 2>/dev/null; then
    test_pass "Unknown exit code 99 rejected"
else
    test_fail "Exit code 99 should be rejected"
fi

# ---------------------------------------------------------------------------
# _needle_validate_exit_code_coverage
# ---------------------------------------------------------------------------

echo ""
echo "Test 13: exit code coverage check passes"
if _needle_validate_exit_code_coverage 2>/dev/null; then
    test_pass "Exit code coverage check passes"
else
    test_fail "Exit code coverage check should pass"
fi

# ---------------------------------------------------------------------------
# _needle_validate_hook_env
# ---------------------------------------------------------------------------

echo ""
echo "Test 14: env validation - required vars present"
export NEEDLE_HOME="$NEEDLE_HOME"
export NEEDLE_CONFIG_FILE="$NEEDLE_CONFIG_FILE"
if _needle_validate_hook_env 2>/dev/null; then
    test_pass "Env validation passes when required vars set"
else
    test_fail "Env validation should pass when required vars set"
fi

echo ""
echo "Test 15: env validation - NEEDLE_HOME missing"
old_home="$NEEDLE_HOME"
unset NEEDLE_HOME
if ! _needle_validate_hook_env 2>/dev/null; then
    test_pass "Env validation fails when NEEDLE_HOME missing"
else
    test_fail "Env validation should fail when NEEDLE_HOME missing"
fi
export NEEDLE_HOME="$old_home"

# ---------------------------------------------------------------------------
# _needle_validate_hook_env_vars
# ---------------------------------------------------------------------------

echo ""
echo "Test 16: env var check - known vars only"
KNOWN_HOOK=$(make_hook "known-vars.sh" 'echo "$NEEDLE_BEAD_ID $NEEDLE_WORKER"')
if _needle_validate_hook_env_vars "$KNOWN_HOOK" 2>/dev/null; then
    test_pass "Known NEEDLE_ vars pass env var check"
else
    test_fail "Known NEEDLE_ vars should pass env var check"
fi

echo ""
echo "Test 17: env var check - unknown vars flagged"
UNKNOWN_HOOK=$(make_hook "unknown-vars.sh" 'echo "$NEEDLE_UNKNOWN_VAR_XYZ"')
if ! _needle_validate_hook_env_vars "$UNKNOWN_HOOK" 2>/dev/null; then
    test_pass "Unknown NEEDLE_ vars flagged by env var check"
else
    test_fail "Unknown NEEDLE_ vars should be flagged"
fi

# ---------------------------------------------------------------------------
# _needle_validate_hook_script
# ---------------------------------------------------------------------------

echo ""
echo "Test 18: validate_hook_script - fully valid script"
GOOD_HOOK=$(make_hook "good.sh" 'echo "$NEEDLE_BEAD_ID"\nexit 0')
if _needle_validate_hook_script "$GOOD_HOOK" 2>/dev/null; then
    test_pass "Valid script passes full validation"
else
    test_fail "Valid script should pass full validation"
fi

echo ""
echo "Test 19: validate_hook_script - non-existent file"
if ! _needle_validate_hook_script "/nonexistent/hook.sh" 2>/dev/null; then
    test_pass "Non-existent file fails full validation"
else
    test_fail "Non-existent file should fail full validation"
fi

echo ""
echo "Test 20: validate_hook_script - not executable"
NONEXEC_HOOK="$NEEDLE_HOME/hooks/nonexec2.sh"
printf '#!/bin/bash\nexit 0\n' > "$NONEXEC_HOOK"
chmod -x "$NONEXEC_HOOK"
if ! _needle_validate_hook_script "$NONEXEC_HOOK" 2>/dev/null; then
    test_pass "Non-executable file fails full validation"
else
    test_fail "Non-executable file should fail full validation"
fi

echo ""
echo "Test 21: validate_hook_script - syntax error"
BAD_SYNTAX_HOOK="$NEEDLE_HOME/hooks/bad-syntax.sh"
printf '#!/bin/bash\nif [ then\nexit 0\n' > "$BAD_SYNTAX_HOOK"
chmod +x "$BAD_SYNTAX_HOOK"
if ! _needle_validate_hook_script "$BAD_SYNTAX_HOOK" 2>/dev/null; then
    test_pass "Syntax error causes full validation failure"
else
    test_fail "Syntax error should cause full validation failure"
fi

# ---------------------------------------------------------------------------
# _needle_validate_all_configured_hooks
# ---------------------------------------------------------------------------

echo ""
echo "Test 22: validate all configured hooks - valid"
GOOD2=$(make_hook "good2.sh" "exit 0")
write_cfg "pre_claim" "$GOOD2"
if _needle_validate_all_configured_hooks 2>/dev/null; then
    test_pass "All configured hooks valid"
else
    test_fail "All configured hooks should be valid"
fi

echo ""
echo "Test 23: validate all configured hooks - missing hook file"
write_cfg "pre_claim" "$NEEDLE_HOME/hooks/missing.sh"
if ! _needle_validate_all_configured_hooks 2>/dev/null; then
    test_pass "Missing hook file causes bulk validation failure"
else
    test_fail "Missing hook file should cause bulk validation failure"
fi

echo ""
echo "Test 24: validate all configured hooks - invalid timeout"
GOOD3=$(make_hook "good3.sh" "exit 0")
cat > "$NEEDLE_CONFIG_FILE" << CFGEOF
hooks:
  timeout: invalid
  fail_action: warn
  pre_claim: $GOOD3
CFGEOF
clear_config_cache
if ! _needle_validate_all_configured_hooks 2>/dev/null; then
    test_pass "Invalid timeout format causes validation failure"
else
    test_fail "Invalid timeout format should cause validation failure"
fi

echo ""
echo "Test 25: validate all configured hooks - invalid fail_action"
cat > "$NEEDLE_CONFIG_FILE" << CFGEOF
hooks:
  timeout: 5s
  fail_action: badvalue
  pre_claim: $GOOD3
CFGEOF
clear_config_cache
if ! _needle_validate_all_configured_hooks 2>/dev/null; then
    test_pass "Invalid fail_action causes validation failure"
else
    test_fail "Invalid fail_action should cause validation failure"
fi

# ---------------------------------------------------------------------------
# _needle_hook_validation_report
# ---------------------------------------------------------------------------

echo ""
echo "Test 26: validation report runs without error"
write_cfg "pre_claim" "$GOOD3"
if _needle_hook_validation_report > /dev/null 2>&1; then
    test_pass "Validation report runs without error"
else
    test_fail "Validation report should run without error"
fi

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
rm -rf "$NEEDLE_HOME"

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
