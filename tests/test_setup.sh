#!/usr/bin/env bash
#
# Tests for src/cli/setup.sh module
#
# Usage: ./tests/test_setup.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
SETUP_MODULE="$PROJECT_ROOT/src/cli/setup.sh"

# Test utilities
tests_run=0
tests_passed=0
tests_failed=0

test_start() {
    ((tests_run++)) || true
    printf "  Testing: %s... " "$1"
}

test_pass() {
    ((tests_passed++)) || true
    printf "%b✓%b\n" "\033[0;32m" "\033[0m"
}

test_fail() {
    ((tests_failed++)) || true
    printf "%b✗%b\n" "\033[0;31m" "\033[0m"
    echo "    Reason: $1"
}

# Source the required modules for testing
source_modules() {
    source "$PROJECT_ROOT/src/lib/constants.sh"
    source "$PROJECT_ROOT/src/lib/output.sh"
    source "$PROJECT_ROOT/bootstrap/check.sh"
    source "$PROJECT_ROOT/bootstrap/install.sh"
    source "$SETUP_MODULE"
}

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: Module file exists
test_module_exists() {
    test_start "Module file exists"
    if [[ -f "$SETUP_MODULE" ]]; then
        test_pass
    else
        test_fail "src/cli/setup.sh not found"
    fi
}

# Test: Module is sourceable
test_module_sourceable() {
    test_start "Module is sourceable"
    if source "$SETUP_MODULE" 2>/dev/null; then
        test_pass
    else
        test_fail "Failed to source module"
    fi
}

# Test: _needle_setup_help function exists
test_setup_help_exists() {
    test_start "_needle_setup_help function exists"
    source_modules
    if declare -f _needle_setup_help &>/dev/null; then
        test_pass
    else
        test_fail "_needle_setup_help function not defined"
    fi
}

# Test: _needle_setup function exists
test_setup_function_exists() {
    test_start "_needle_setup function exists"
    source_modules
    if declare -f _needle_setup &>/dev/null; then
        test_pass
    else
        test_fail "_needle_setup function not defined"
    fi
}

# Test: _needle_setup_json function exists
test_setup_json_exists() {
    test_start "_needle_setup_json function exists"
    source_modules
    if declare -f _needle_setup_json &>/dev/null; then
        test_pass
    else
        test_fail "_needle_setup_json function not defined"
    fi
}

# Test: _needle_setup_json outputs valid JSON
test_setup_json_output() {
    test_start "_needle_setup_json outputs valid JSON"
    source_modules

    local output
    output=$(_needle_setup_json 2>/dev/null)

    # Check for valid JSON structure
    if echo "$output" | grep -q '^{' && \
       echo "$output" | grep -q '}$' && \
       echo "$output" | grep -q '"status"'; then
        test_pass
    else
        test_fail "Invalid JSON output"
    fi
}

# Test: Help output contains expected sections
test_help_content() {
    test_start "Help output contains expected sections"
    source_modules

    local output
    output=$(_needle_setup_help 2>/dev/null)

    if echo "$output" | grep -q "USAGE:" && \
       echo "$output" | grep -q "OPTIONS:" && \
       echo "$output" | grep -q "\-\-check" && \
       echo "$output" | grep -q "\-\-reinstall" && \
       echo "$output" | grep -q "\-\-yes" && \
       echo "$output" | grep -q "\-\-json"; then
        test_pass
    else
        test_fail "Missing expected help content"
    fi
}

# Test: NEEDLE_EXIT_DEPENDENCY is defined
test_exit_dependency_defined() {
    test_start "NEEDLE_EXIT_DEPENDENCY is defined"
    source_modules
    if [[ -n "${NEEDLE_EXIT_DEPENDENCY:-}" ]]; then
        test_pass
    else
        test_fail "NEEDLE_EXIT_DEPENDENCY not defined"
    fi
}

# Test: NEEDLE_EXIT_CANCELLED is defined
test_exit_cancelled_defined() {
    test_start "NEEDLE_EXIT_CANCELLED is defined"
    source_modules
    if [[ -n "${NEEDLE_EXIT_CANCELLED:-}" ]]; then
        test_pass
    else
        test_fail "NEEDLE_EXIT_CANCELLED not defined"
    fi
}

# Test: CLI integration - help works
test_cli_help() {
    test_start "CLI integration - help works"
    local output
    output=$("$PROJECT_ROOT/bin/needle" setup --help 2>&1)
    if echo "$output" | grep -q "Check and install NEEDLE dependencies"; then
        test_pass
    else
        test_fail "CLI help command failed"
    fi
}

# Test: CLI integration - check exits properly
test_cli_check() {
    test_start "CLI integration - check command exits properly"

    # Run the check command in a subshell to capture exit code
    local exit_code
    exit_code=$("$PROJECT_ROOT/bin/needle" setup --check >/dev/null 2>&1; echo $?)

    # Exit code should be either 0 (all deps ok) or 5 (missing deps)
    if [[ $exit_code -eq 0 || $exit_code -eq 5 ]]; then
        test_pass
    else
        test_fail "Unexpected exit code: $exit_code"
    fi
}

# Test: CLI integration - JSON output works
test_cli_json() {
    test_start "CLI integration - JSON output works"
    local output
    output=$("$PROJECT_ROOT/bin/needle" setup --json 2>/dev/null)

    if echo "$output" | jq -e . >/dev/null 2>&1; then
        test_pass
    else
        test_fail "JSON output is not valid JSON"
    fi
}

# Test: Setup command is in NEEDLE_SUBCOMMANDS
test_in_subcommands() {
    test_start "Setup command is in NEEDLE_SUBCOMMANDS"
    source_modules

    if [[ " ${NEEDLE_SUBCOMMANDS[*]} " == *" setup "* ]]; then
        test_pass
    else
        test_fail "setup not in NEEDLE_SUBCOMMANDS"
    fi
}

# Test: Setup is in no-init commands list
test_no_init_required() {
    test_start "Setup command doesn't require init"
    # This is tested by verifying that setup --help works even without .needle/config.yaml
    local temp_home
    temp_home=$(mktemp -d)

    local output
    output=$(NEEDLE_HOME="$temp_home" "$PROJECT_ROOT/bin/needle" setup --help 2>&1)
    local exit_code=$?

    rm -rf "$temp_home"

    if [[ $exit_code -eq 0 ]] && echo "$output" | grep -q "Check and install"; then
        test_pass
    else
        test_fail "setup should work without initialization"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Setup CLI Module Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_module_exists
test_module_sourceable
test_setup_help_exists
test_setup_function_exists
test_setup_json_exists
test_setup_json_output
test_help_content
test_exit_dependency_defined
test_exit_cancelled_defined
test_cli_help
test_cli_check
test_cli_json
test_in_subcommands
test_no_init_required

# Summary
printf "\n%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "Tests: %d total, %b%d passed%b, %b%d failed%b\n" \
    "$tests_run" \
    "\033[0;32m" "$tests_passed" "\033[0m" \
    "\033[0;31m" "$tests_failed" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"

# Exit with appropriate code
if [[ $tests_failed -gt 0 ]]; then
    exit 1
fi
exit 0
