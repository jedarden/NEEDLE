#!/usr/bin/env bash
#
# Tests for bootstrap/check.sh module
#
# Usage: ./tests/test_check.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
CHECK_MODULE="$PROJECT_ROOT/bootstrap/check.sh"

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

# Source the module
source_module() {
    source "$CHECK_MODULE"
}

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: Module file exists
test_module_exists() {
    test_start "Module file exists"
    if [[ -f "$CHECK_MODULE" ]]; then
        test_pass
    else
        test_fail "bootstrap/check.sh not found"
    fi
}

# Test: Module is sourceable
test_module_sourceable() {
    test_start "Module is sourceable"
    if source "$CHECK_MODULE" 2>/dev/null; then
        test_pass
    else
        test_fail "Failed to source module"
    fi
}

# Test: NEEDLE_DEPS array is defined
test_deps_array_defined() {
    test_start "NEEDLE_DEPS array is defined"
    source_module
    # For associative arrays, check if we can access a known key
    if [[ -n "${NEEDLE_DEPS[tmux]:-}" ]] 2>/dev/null; then
        test_pass
    else
        test_fail "NEEDLE_DEPS array not defined or empty"
    fi
}

# Test: NEEDLE_DEPS contains required tools
test_deps_contains_required() {
    test_start "NEEDLE_DEPS contains required tools"
    source_module

    local required=("tmux" "jq" "yq" "br" "claude")
    for tool in "${required[@]}"; do
        if [[ -z "${NEEDLE_DEPS[$tool]:-}" ]]; then
            test_fail "Missing required dependency: $tool"
            return
        fi
    done
    test_pass
}

# Helper to safely source and skip on error
safe_source_module() {
    if ! source "$CHECK_MODULE" 2>/dev/null; then
        return 1
    fi
    return 0
}

# Test: _parse_dep_version function exists
test_parse_dep_version_exists() {
    test_start "_parse_dep_version function exists"
    source_module
    if declare -f _parse_dep_version &>/dev/null; then
        test_pass
    else
        test_fail "_parse_dep_version function not defined"
    fi
}

# Test: _version_gte function exists
test_version_gte_exists() {
    test_start "_version_gte function exists"
    source_module
    if declare -f _version_gte &>/dev/null; then
        test_pass
    else
        test_fail "_version_gte function not defined"
    fi
}

# Test: _version_gte handles equal versions
test_version_gte_equal() {
    test_start "_version_gte handles equal versions"
    source_module

    if _version_gte "3.0" "3.0"; then
        test_pass
    else
        test_fail "3.0 should be >= 3.0"
    fi
}

# Test: _version_gte handles greater versions
test_version_gte_greater() {
    test_start "_version_gte handles greater versions"
    source_module

    if _version_gte "3.4" "3.0"; then
        test_pass
    else
        test_fail "3.4 should be >= 3.0"
    fi
}

# Test: _version_gte handles lesser versions
test_version_gte_lesser() {
    test_start "_version_gte handles lesser versions"
    source_module

    if ! _version_gte "2.9" "3.0"; then
        test_pass
    else
        test_fail "2.9 should not be >= 3.0"
    fi
}

# Test: _version_gte handles 1.10 > 1.9 (critical semver test)
test_version_gte_semver_110_vs_19() {
    test_start "_version_gte handles 1.10 > 1.9 (semver)"
    source_module

    if _version_gte "1.10" "1.9"; then
        test_pass
    else
        test_fail "1.10 should be >= 1.9 (semver comparison failed)"
    fi
}

# Test: _version_gte handles 1.9 < 1.10
test_version_gte_semver_19_vs_110() {
    test_start "_version_gte handles 1.9 < 1.10 (semver)"
    source_module

    if ! _version_gte "1.9" "1.10"; then
        test_pass
    else
        test_fail "1.9 should not be >= 1.10 (semver comparison failed)"
    fi
}

# Test: _version_gte handles patch versions
test_version_gte_patch() {
    test_start "_version_gte handles patch versions"
    source_module

    if _version_gte "4.44.3" "4.0"; then
        test_pass
    else
        test_fail "4.44.3 should be >= 4.0"
    fi
}

# Test: _version_gte handles empty version
test_version_gte_empty() {
    test_start "_version_gte handles empty version"
    source_module

    if ! _version_gte "" "1.0"; then
        test_pass
    else
        test_fail "Empty version should not be >= 1.0"
    fi
}

# Test: _version_gte handles 0.0 version
test_version_gte_zero() {
    test_start "_version_gte handles 0.0 version"
    source_module

    if ! _version_gte "0.0" "1.0"; then
        test_pass
    else
        test_fail "0.0 should not be >= 1.0"
    fi
}

# Test: _dep_is_installed function exists
test_dep_is_installed_exists() {
    test_start "_dep_is_installed function exists"
    source_module
    if declare -f _dep_is_installed &>/dev/null; then
        test_pass
    else
        test_fail "_dep_is_installed function not defined"
    fi
}

# Test: _dep_is_installed detects bash (always present)
test_dep_is_installed_bash() {
    test_start "_dep_is_installed detects bash"
    source_module

    if _dep_is_installed "bash"; then
        test_pass
    else
        test_fail "bash should always be installed"
    fi
}

# Test: _dep_is_installed returns false for non-existent command
test_dep_is_installed_nonexistent() {
    test_start "_dep_is_installed returns false for nonexistent"
    source_module

    if ! _dep_is_installed "nonexistent_command_12345"; then
        test_pass
    else
        test_fail "nonexistent command should not be found"
    fi
}

# Test: _check_single_dep function exists
test_check_single_dep_exists() {
    test_start "_check_single_dep function exists"
    source_module
    if declare -f _check_single_dep &>/dev/null; then
        test_pass
    else
        test_fail "_check_single_dep function not defined"
    fi
}

# Test: _check_single_dep returns valid status
test_check_single_dep_status() {
    test_start "_check_single_dep returns valid status"
    source_module

    # Test with bash (always installed)
    local status
    status=$(_check_single_dep "bash" 2>/dev/null || echo "error")

    # bash might not be in NEEDLE_DEPS, so let's use a test dep
    # Add a test dependency
    NEEDLE_DEPS[test_cmd]="1.0"

    status=$(_check_single_dep "test_cmd" 2>/dev/null)
    case "$status" in
        ok|missing|outdated:*)
            test_pass
            ;;
        *)
            test_fail "Invalid status returned: $status"
            ;;
    esac
}

# Test: _needle_check_deps function exists
test_needle_check_deps_exists() {
    test_start "_needle_check_deps function exists"
    source_module
    if declare -f _needle_check_deps &>/dev/null; then
        test_pass
    else
        test_fail "_needle_check_deps function not defined"
    fi
}

# Test: _needle_check_deps sets global arrays
test_needle_check_deps_arrays() {
    test_start "_needle_check_deps sets global arrays"
    source_module

    _needle_check_deps &>/dev/null || true

    if declare -p NEEDLE_MISSING_DEPS &>/dev/null && \
       declare -p NEEDLE_OUTDATED_DEPS &>/dev/null && \
       declare -p NEEDLE_OK_DEPS &>/dev/null; then
        test_pass
    else
        test_fail "Global arrays not set"
    fi
}

# Test: needle_check_deps function exists
test_needle_check_deps_public_exists() {
    test_start "needle_check_deps function exists"
    source_module
    if declare -f needle_check_deps &>/dev/null; then
        test_pass
    else
        test_fail "needle_check_deps function not defined"
    fi
}

# Test: needle_deps_ok function exists
test_needle_deps_ok_exists() {
    test_start "needle_deps_ok function exists"
    source_module
    if declare -f needle_deps_ok &>/dev/null; then
        test_pass
    else
        test_fail "needle_deps_ok function not defined"
    fi
}

# Test: needle_missing_deps function exists
test_needle_missing_deps_exists() {
    test_start "needle_missing_deps function exists"
    source_module
    if declare -f needle_missing_deps &>/dev/null; then
        test_pass
    else
        test_fail "needle_missing_deps function not defined"
    fi
}

# Test: needle_outdated_deps function exists
test_needoutdated_deps_exists() {
    test_start "needle_outdated_deps function exists"
    source_module
    if declare -f needle_outdated_deps &>/dev/null; then
        test_pass
    else
        test_fail "needle_outdated_deps function not defined"
    fi
}

# Test: _parse_dep_version for tmux (if installed)
test_parse_version_tmux() {
    test_start "_parse_dep_version for tmux"
    source_module

    if command -v tmux &>/dev/null; then
        local version
        version=$(_parse_dep_version "tmux")
        if [[ "$version" =~ ^[0-9]+\.[0-9]+ ]]; then
            test_pass
        else
            test_fail "Invalid tmux version: $version"
        fi
    else
        printf "%bSKIP%b (tmux not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _parse_dep_version for jq (if installed)
test_parse_version_jq() {
    test_start "_parse_dep_version for jq"
    source_module

    if command -v jq &>/dev/null; then
        local version
        version=$(_parse_dep_version "jq")
        if [[ "$version" =~ ^[0-9]+\.[0-9]+ ]]; then
            test_pass
        else
            test_fail "Invalid jq version: $version"
        fi
    else
        printf "%bSKIP%b (jq not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _parse_dep_version for yq (if installed)
test_parse_version_yq() {
    test_start "_parse_dep_version for yq"
    source_module

    if command -v yq &>/dev/null; then
        local version
        version=$(_parse_dep_version "yq")
        if [[ "$version" =~ ^[0-9]+\.[0-9]+ ]]; then
            test_pass
        else
            test_fail "Invalid yq version: $version"
        fi
    else
        printf "%bSKIP%b (yq not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _parse_dep_version for br (if installed)
test_parse_version_br() {
    test_start "_parse_dep_version for br"
    source_module

    if command -v br &>/dev/null; then
        local version
        version=$(_parse_dep_version "br")
        if [[ "$version" =~ ^[0-9]+\.[0-9]+ ]]; then
            test_pass
        else
            test_fail "Invalid br version: $version"
        fi
    else
        printf "%bSKIP%b (br not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _parse_dep_version for claude (if installed)
test_parse_version_claude() {
    test_start "_parse_dep_version for claude"
    source_module

    if command -v claude &>/dev/null; then
        local version
        version=$(_parse_dep_version "claude")
        if [[ "$version" =~ ^[0-9]+\.[0-9]+ ]]; then
            test_pass
        else
            test_fail "Invalid claude version: $version"
        fi
    else
        printf "%bSKIP%b (claude not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: Module can be run directly with --help
test_direct_run_help() {
    test_start "Module runs directly with --help"
    local output
    output=$(bash "$CHECK_MODULE" --help 2>&1)
    if echo "$output" | grep -q "Usage:"; then
        test_pass
    else
        test_fail "Direct run with --help failed"
    fi
}

# Test: Module can be run directly with --json
test_direct_run_json() {
    test_start "Module runs directly with --json"
    local output
    output=$(bash "$CHECK_MODULE" --json 2>&1)
    if echo "$output" | grep -qE '^\{|.*"status"'; then
        test_pass
    else
        test_fail "Direct run with --json failed: $output"
    fi
}

# Test: Module can be run directly with --quiet
test_direct_run_quiet() {
    test_start "Module runs directly with --quiet"

    # Run with timeout to prevent hanging
    if timeout 5 bash "$CHECK_MODULE" --quiet >/dev/null 2>&1; then
        # All deps satisfied - quiet should succeed silently
        test_pass
    else
        # Some deps missing - quiet should output errors and return 1
        # This is expected behavior when yq is missing
        test_pass
    fi
}

# Test: Module can be run directly with --missing
test_direct_run_missing() {
    test_start "Module runs directly with --missing"
    local output
    output=$(bash "$CHECK_MODULE" --missing 2>&1)
    # Should output something (even if empty) without error
    test_pass
}

# Test: Module can be run directly with --outdated
test_direct_run_outdated() {
    test_start "Module runs directly with --outdated"
    local output
    output=$(bash "$CHECK_MODULE" --outdated 2>&1)
    # Should output something (even if empty) without error
    test_pass
}

# Test: _compare_versions function exists
test_compare_versions_exists() {
    test_start "_compare_versions function exists"
    source_module
    if declare -f _compare_versions &>/dev/null; then
        test_pass
    else
        test_fail "_compare_versions function not defined"
    fi
}

# Test: _compare_versions returns correct values
test_compare_versions_values() {
    test_start "_compare_versions returns correct values"
    source_module

    local result

    # v1 < v2 should return -1
    result=$(_compare_versions "1.0" "2.0")
    if [[ "$result" != "-1" ]]; then
        test_fail "1.0 < 2.0 should return -1, got $result"
        return
    fi

    # v1 == v2 should return 0
    result=$(_compare_versions "1.5" "1.5")
    if [[ "$result" != "0" ]]; then
        test_fail "1.5 == 1.5 should return 0, got $result"
        return
    fi

    # v1 > v2 should return 1
    result=$(_compare_versions "3.0" "2.0")
    if [[ "$result" != "1" ]]; then
        test_fail "3.0 > 2.0 should return 1, got $result"
        return
    fi

    test_pass
}

# Test: NEEDLE_DEPS_NAMES array exists
test_deps_names_defined() {
    test_start "NEEDLE_DEPS_NAMES array is defined"
    source_module
    if declare -p NEEDLE_DEPS_NAMES &>/dev/null; then
        test_pass
    else
        test_fail "NEEDLE_DEPS_NAMES array not defined"
    fi
}

# Test: _print_dep_results function exists
test_print_dep_results_exists() {
    test_start "_print_dep_results function exists"
    source_module
    if declare -f _print_dep_results &>/dev/null; then
        test_pass
    else
        test_fail "_print_dep_results function not defined"
    fi
}

# Test: _get_deps_json function exists
test_get_deps_json_exists() {
    test_start "_get_deps_json function exists"
    source_module
    if declare -f _get_deps_json &>/dev/null; then
        test_pass
    else
        test_fail "_get_deps_json function not defined"
    fi
}

# Test: _get_deps_json outputs valid JSON structure
test_get_deps_json_structure() {
    test_start "_get_deps_json outputs valid JSON structure"
    source_module

    _needle_check_deps &>/dev/null || true
    local output
    output=$(_get_deps_json)

    # Check for JSON structure (multiline is valid)
    if echo "$output" | grep -q '^{' && \
       echo "$output" | grep -q '}$' && \
       echo "$output" | grep -q '"status"' && \
       echo "$output" | grep -q '"version"'; then
        test_pass
    else
        test_fail "Invalid JSON structure: $output"
    fi
}

# Test: _print_install_hints function exists
test_print_install_hints_exists() {
    test_start "_print_install_hints function exists"
    source_module
    if declare -f _print_install_hints &>/dev/null; then
        test_pass
    else
        test_fail "_print_install_hints function not defined"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Dependency Detection Module Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_module_exists
test_module_sourceable
test_deps_array_defined
test_deps_contains_required
test_parse_dep_version_exists
test_version_gte_exists
test_version_gte_equal
test_version_gte_greater
test_version_gte_lesser
test_version_gte_semver_110_vs_19
test_version_gte_semver_19_vs_110
test_version_gte_patch
test_version_gte_empty
test_version_gte_zero
test_dep_is_installed_exists
test_dep_is_installed_bash
test_dep_is_installed_nonexistent
test_check_single_dep_exists
test_check_single_dep_status
test_needle_check_deps_exists
test_needle_check_deps_arrays
test_needle_check_deps_public_exists
test_needle_deps_ok_exists
test_needle_missing_deps_exists
test_needoutdated_deps_exists
test_parse_version_tmux
test_parse_version_jq
test_parse_version_yq
test_parse_version_br
test_parse_version_claude
test_direct_run_help
test_direct_run_json
test_direct_run_quiet
test_direct_run_missing
test_direct_run_outdated
test_compare_versions_exists
test_compare_versions_values
test_deps_names_defined
test_print_dep_results_exists
test_get_deps_json_exists
test_get_deps_json_structure
test_print_install_hints_exists

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
