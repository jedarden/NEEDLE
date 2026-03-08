#!/usr/bin/env bash
#
# Tests for needle init dependency checker
#
# Usage: ./tests/test_init_check_deps.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

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

# Source required modules
source_modules() {
    source "$PROJECT_ROOT/src/lib/constants.sh"
    source "$PROJECT_ROOT/src/lib/output.sh"
    source "$PROJECT_ROOT/bootstrap/check.sh"
    source "$PROJECT_ROOT/src/cli/init.sh"
}

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: _needle_init_check_deps function exists
test_init_check_deps_exists() {
    test_start "_needle_init_check_deps function exists"
    source_modules

    if declare -f _needle_init_check_deps &>/dev/null; then
        test_pass
    else
        test_fail "_needle_init_check_deps function not defined"
    fi
}

# Test: _needle_init_check_deps outputs correct header
test_init_check_deps_header() {
    test_start "_needle_init_check_deps outputs correct header"
    source_modules

    local output
    output=$(_needle_init_check_deps 2>&1 || true)

    if echo "$output" | grep -q "Checking dependencies..."; then
        test_pass
    else
        test_fail "Missing 'Checking dependencies...' header"
    fi
}

# Test: _needle_init_check_deps shows installed status for present deps
test_init_check_deps_installed_format() {
    test_start "_needle_init_check_deps shows installed format"
    source_modules

    local output
    output=$(_needle_init_check_deps 2>&1 || true)

    # Check for ✓ and "installed" text
    if echo "$output" | grep -q "installed"; then
        test_pass
    else
        test_fail "Missing 'installed' status text"
    fi
}

# Test: _needle_init_check_deps shows NOT FOUND for missing deps
test_init_check_deps_missing_format() {
    test_start "_needle_init_check_deps shows NOT FOUND format"
    source_modules

    # Run check and capture output
    local output exit_code
    output=$(_needle_init_check_deps 2>&1 || true)
    _needle_init_check_deps &>/dev/null && exit_code=0 || exit_code=$?

    if echo "$output" | grep -q "NOT FOUND"; then
        test_pass
    elif [[ "$exit_code" -eq 0 ]]; then
        # All deps present - skip this test
        printf "%bSKIP%b (all deps installed)\n" "\033[0;33m" "\033[0m"
    else
        test_fail "Missing 'NOT FOUND' status text for missing deps"
    fi
}

# Test: _needle_init_check_deps shows missing deps list
test_init_check_deps_missing_list() {
    test_start "_needle_init_check_deps shows missing deps list"
    source_modules

    local output exit_code
    output=$(_needle_init_check_deps 2>&1 || true)
    _needle_init_check_deps &>/dev/null && exit_code=0 || exit_code=$?

    if echo "$output" | grep -q "Missing dependencies:"; then
        test_pass
    elif [[ "$exit_code" -eq 0 ]]; then
        # All deps present - skip
        printf "%bSKIP%b (all deps installed)\n" "\033[0;33m" "\033[0m"
    else
        test_fail "Missing 'Missing dependencies:' message"
    fi
}

# Test: _needle_init_check_deps suggests needle setup
test_init_check_deps_setup_hint() {
    test_start "_needle_init_check_deps suggests needle setup"
    source_modules

    local output exit_code
    output=$(_needle_init_check_deps 2>&1 || true)
    _needle_init_check_deps &>/dev/null && exit_code=0 || exit_code=$?

    if echo "$output" | grep -q "needle setup"; then
        test_pass
    elif [[ "$exit_code" -eq 0 ]]; then
        # All deps present - skip
        printf "%bSKIP%b (all deps installed)\n" "\033[0;33m" "\033[0m"
    else
        test_fail "Missing 'needle setup' suggestion"
    fi
}

# Test: _needle_init_check_deps returns 0 when all deps present
test_init_check_deps_exit_success() {
    test_start "_needle_init_check_deps returns 0 when all deps present"
    source_modules

    # First check if all deps are actually present
    if _needle_check_deps &>/dev/null; then
        if _needle_init_check_deps &>/dev/null; then
            test_pass
        else
            test_fail "Should return 0 when all deps present"
        fi
    else
        printf "%bSKIP%b (missing deps)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _needle_init_check_deps returns 1 when deps missing
test_init_check_deps_exit_failure() {
    test_start "_needle_init_check_deps returns 1 when deps missing"
    source_modules

    # Check if any deps are missing
    if ! _needle_check_deps &>/dev/null; then
        if ! _needle_init_check_deps &>/dev/null; then
            test_pass
        else
            test_fail "Should return 1 when deps are missing"
        fi
    else
        printf "%bSKIP%b (all deps present)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _needle_init_check_deps checks all required tools
test_init_check_deps_checks_all_tools() {
    test_start "_needle_init_check_deps checks all required tools"
    source_modules

    local output
    output=$(_needle_init_check_deps 2>&1 || true)

    # Check for all required tools in output
    local tools=("br" "yq" "jq" "tmux" "claude")
    for tool in "${tools[@]}"; do
        if ! echo "$output" | grep -q "$tool"; then
            test_fail "Missing check for $tool"
            return
        fi
    done

    test_pass
}

# Test: _needle_init_check_deps shows version for installed deps
test_init_check_deps_shows_version() {
    test_start "_needle_init_check_deps shows version for installed deps"
    source_modules

    local output
    output=$(_needle_init_check_deps 2>&1 || true)

    # Version should be in format like "0.1.20" or "3.4"
    if echo "$output" | grep -qE "[0-9]+\.[0-9]+"; then
        test_pass
    else
        test_fail "Missing version numbers in output"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Init Dependency Checker Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_init_check_deps_exists
test_init_check_deps_header
test_init_check_deps_installed_format
test_init_check_deps_missing_format
test_init_check_deps_missing_list
test_init_check_deps_setup_hint
test_init_check_deps_exit_success
test_init_check_deps_exit_failure
test_init_check_deps_checks_all_tools
test_init_check_deps_shows_version

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
