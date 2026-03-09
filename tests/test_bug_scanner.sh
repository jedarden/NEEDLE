#!/usr/bin/env bash
#
# Tests for quality/bug_scanner.sh module
#
# Usage: ./tests/test_bug_scanner.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
BUG_SCANNER_MODULE="$PROJECT_ROOT/src/quality/bug_scanner.sh"
INSTALL_MODULE="$PROJECT_ROOT/bootstrap/install.sh"
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

test_skip() {
    printf "%bSKIP%b (%s)\n" "\033[0;33m" "\033[0m" "$1"
}

# Source the module
source_module() {
    # Source dependencies first
    source "$PROJECT_ROOT/src/lib/output.sh"
    source "$BUG_SCANNER_MODULE"
}

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: Module file exists
test_module_exists() {
    test_start "Module file exists"
    if [[ -f "$BUG_SCANNER_MODULE" ]]; then
        test_pass
    else
        test_fail "src/quality/bug_scanner.sh not found"
    fi
}

# Test: Module is sourceable
test_module_sourceable() {
    test_start "Module is sourceable"
    if source "$BUG_SCANNER_MODULE" 2>/dev/null; then
        test_pass
    else
        test_fail "Failed to source module"
    fi
}

# Test: Quality directory exists
test_quality_directory_exists() {
    test_start "Quality directory exists"
    if [[ -d "$PROJECT_ROOT/src/quality" ]]; then
        test_pass
    else
        test_fail "src/quality directory not found"
    fi
}

# Test: _bug_scanner_available function exists
test_bug_scanner_available_exists() {
    test_start "_bug_scanner_available function exists"
    source_module
    if declare -f _bug_scanner_available &>/dev/null; then
        test_pass
    else
        test_fail "_bug_scanner_available function not defined"
    fi
}

# Test: _bug_scanner_severity_level function exists
test_severity_level_exists() {
    test_start "_bug_scanner_severity_level function exists"
    source_module
    if declare -f _bug_scanner_severity_level &>/dev/null; then
        test_pass
    else
        test_fail "_bug_scanner_severity_level function not defined"
    fi
}

# Test: _bug_scanner_meets_threshold function exists
test_meets_threshold_exists() {
    test_start "_bug_scanner_meets_threshold function exists"
    source_module
    if declare -f _bug_scanner_meets_threshold &>/dev/null; then
        test_pass
    else
        test_fail "_bug_scanner_meets_threshold function not defined"
    fi
}

# Test: bug_scanner_scan_bead function exists
test_scan_bead_exists() {
    test_start "bug_scanner_scan_bead function exists"
    source_module
    if declare -f bug_scanner_scan_bead &>/dev/null; then
        test_pass
    else
        test_fail "bug_scanner_scan_bead function not defined"
    fi
}

# Test: bug_scanner_quick_check function exists
test_quick_check_exists() {
    test_start "bug_scanner_quick_check function exists"
    source_module
    if declare -f bug_scanner_quick_check &>/dev/null; then
        test_pass
    else
        test_fail "bug_scanner_quick_check function not defined"
    fi
}

# Test: bug_scanner_status function exists
test_status_exists() {
    test_start "bug_scanner_status function exists"
    source_module
    if declare -f bug_scanner_status &>/dev/null; then
        test_pass
    else
        test_fail "bug_scanner_status function not defined"
    fi
}

# Test: Severity level comparison - critical
test_severity_level_critical() {
    test_start "Severity level for critical is 3"
    source_module

    local level
    level=$(_bug_scanner_severity_level "critical")

    if [[ "$level" -eq 3 ]]; then
        test_pass
    else
        test_fail "Expected 3, got $level"
    fi
}

# Test: Severity level comparison - error
test_severity_level_error() {
    test_start "Severity level for error is 2"
    source_module

    local level
    level=$(_bug_scanner_severity_level "error")

    if [[ "$level" -eq 2 ]]; then
        test_pass
    else
        test_fail "Expected 2, got $level"
    fi
}

# Test: Severity level comparison - warning
test_severity_level_warning() {
    test_start "Severity level for warning is 1"
    source_module

    local level
    level=$(_bug_scanner_severity_level "warning")

    if [[ "$level" -eq 1 ]]; then
        test_pass
    else
        test_fail "Expected 1, got $level"
    fi
}

# Test: Severity level comparison - info
test_severity_level_info() {
    test_start "Severity level for info is 0"
    source_module

    local level
    level=$(_bug_scanner_severity_level "info")

    if [[ "$level" -eq 0 ]]; then
        test_pass
    else
        test_fail "Expected 0, got $level"
    fi
}

# Test: Severity level comparison - case insensitive
test_severity_level_case_insensitive() {
    test_start "Severity level comparison is case insensitive"
    source_module

    local level1 level2 level3
    level1=$(_bug_scanner_severity_level "CRITICAL")
    level2=$(_bug_scanner_severity_level "Error")
    level3=$(_bug_scanner_severity_level "WARNING")

    if [[ "$level1" -eq 3 && "$level2" -eq 2 && "$level3" -eq 1 ]]; then
        test_pass
    else
        test_fail "Case insensitive comparison failed: crit=$level1 err=$level2 warn=$level3"
    fi
}

# Test: Meets threshold - critical meets error threshold
test_meets_threshold_critical_error() {
    test_start "Critical severity meets error threshold"
    source_module

    if _bug_scanner_meets_threshold "critical" "error"; then
        test_pass
    else
        test_fail "Critical should meet error threshold"
    fi
}

# Test: Meets threshold - warning does not meet error threshold
test_meets_threshold_warning_error() {
    test_start "Warning does not meet error threshold"
    source_module

    if ! _bug_scanner_meets_threshold "warning" "error"; then
        test_pass
    else
        test_fail "Warning should not meet error threshold"
    fi
}

# Test: Meets threshold - info meets info threshold
test_meets_threshold_info_info() {
    test_start "Info meets info threshold"
    source_module

    if _bug_scanner_meets_threshold "info" "info"; then
        test_pass
    else
        test_fail "Info should meet info threshold"
    fi
}

# Test: Meets threshold - error meets warning threshold
test_meets_threshold_error_warning() {
    test_start "Error meets warning threshold"
    source_module

    if _bug_scanner_meets_threshold "error" "warning"; then
        test_pass
    else
        test_fail "Error should meet warning threshold"
    fi
}

# Test: bug_scanner_status returns valid JSON
test_status_returns_json() {
    test_start "bug_scanner_status returns valid JSON"
    source_module

    local output
    output=$(bug_scanner_status)

    if echo "$output" | grep -q '"enabled"' && \
       echo "$output" | grep -q '"available"' && \
       echo "$output" | grep -q '"severity_threshold"'; then
        test_pass
    else
        test_fail "Status output is not valid JSON: $output"
    fi
}

# Test: Module can be run directly with --help
test_direct_run_help() {
    test_start "Module runs directly with --help"
    local output
    output=$(bash "$BUG_SCANNER_MODULE" --help 2>&1)
    if echo "$output" | grep -q "Usage:"; then
        test_pass
    else
        test_fail "Direct run with --help failed"
    fi
}

# Test: Module can be run directly with --status
test_direct_run_status() {
    test_start "Module runs directly with --status"
    local output
    output=$(bash "$BUG_SCANNER_MODULE" --status 2>&1)
    if echo "$output" | grep -q '"enabled"'; then
        test_pass
    else
        test_fail "Direct run with --status failed"
    fi
}

# Test: Module variables are set
test_module_variables_set() {
    test_start "Module variables are set"
    source_module

    if [[ -n "${_NFEDLE_BUG_SCANNER_VERSION:-}" ]] && \
       [[ "${_NFEDLE_BUG_SCANNER_LOADED:-}" == "1" ]]; then
        test_pass
    else
        test_fail "Module variables not set properly"
    fi
}

# Test: BUG_SCANNER_ENABLED default
test_default_enabled() {
    test_start "BUG_SCANNER_ENABLED defaults to true"
    source_module

    if [[ "$BUG_SCANNER_ENABLED" == "true" ]]; then
        test_pass
    else
        test_fail "BUG_SCANNER_ENABLED should default to true, got: $BUG_SCANNER_ENABLED"
    fi
}

# Test: BUG_SCANNER_SEVERITY_THRESHOLD default
test_default_severity() {
    test_start "BUG_SCANNER_SEVERITY_THRESHOLD defaults to error"
    source_module

    if [[ "$BUG_SCANNER_SEVERITY_THRESHOLD" == "error" ]]; then
        test_pass
    else
        test_fail "BUG_SCANNER_SEVERITY_THRESHOLD should default to error, got: $BUG_SCANNER_SEVERITY_THRESHOLD"
    fi
}

# Test: BUG_SCANNER_FAIL_ON_ISSUES default
test_default_fail_on_issues() {
    test_start "BUG_SCANNER_FAIL_ON_ISSUES defaults to true"
    source_module

    if [[ "$BUG_SCANNER_FAIL_ON_ISSUES" == "true" ]]; then
        test_pass
    else
        test_fail "BUG_SCANNER_FAIL_ON_ISSUES should default to true, got: $BUG_SCANNER_FAIL_ON_ISSUES"
    fi
}

# Test: BUG_SCANNER_TIMEOUT default
test_default_timeout() {
    test_start "BUG_SCANNER_TIMEOUT defaults to 300"
    source_module

    if [[ "$BUG_SCANNER_TIMEOUT" == "300" ]]; then
        test_pass
    else
        test_fail "BUG_SCANNER_TIMEOUT should default to 300, got: $BUG_SCANNER_TIMEOUT"
    fi
}

# Test: Module sources output.sh
test_sources_output() {
    test_start "Module sources output.sh"
    source_module

    if declare -f _needle_info &>/dev/null && \
       declare -f _needle_warn &>/dev/null && \
       declare -f _needle_error &>/dev/null; then
        test_pass
    else
        test_fail "output.sh functions not available"
    fi
}

# Test: _bug_scanner_available returns false when ubs not installed
test_available_returns_false() {
    test_start "_bug_scanner_available returns false when ubs not installed"
    source_module

    # Temporarily modify PATH to hide ubs
    local old_path="$PATH"
    export PATH="/usr/bin:/bin"

    if ! _bug_scanner_available; then
        test_pass
    else
        test_fail "_bug_scanner_available should return false when ubs not in PATH"
    fi

    export PATH="$old_path"
}

# Test: Install module has ubs installer function
test_install_ubs_exists() {
    test_start "bootstrap/install.sh has _needle_install_ubs function"
    if grep -q "_needle_install_ubs()" "$INSTALL_MODULE"; then
        test_pass
    else
        test_fail "_needle_install_ubs function not found in install.sh"
    fi
}

# Test: Install module includes ubs in dispatcher
test_install_ubs_dispatch() {
    test_start "bootstrap/install.sh dispatches ubs installer"
    if grep -q 'ubs)' "$INSTALL_MODULE"; then
        test_pass
    else
        test_fail "ubs case not found in installer dispatcher"
    fi
}

# Test: Check module includes ubs dependency
test_check_ubs_defined() {
    test_start "bootstrap/check.sh defines ubs dependency"
    if grep -q '\[ubs\]' "$CHECK_MODULE"; then
        test_pass
    else
        test_fail "ubs not found in NEEDLE_DEPS array"
    fi
}

# Test: Check module has ubs version parsing
test_check_ubs_version() {
    test_start "bootstrap/check.sh has ubs version parsing"
    if grep -q 'ubs)' "$CHECK_MODULE"; then
        test_pass
    else
        test_fail "ubs version parsing not found"
    fi
}

# Test: Config module has bug_scanner config
test_config_bug_scanner() {
    test_start "config.sh includes bug_scanner configuration"
    if grep -q '"bug_scanner"' "$PROJECT_ROOT/src/lib/config.sh"; then
        test_pass
    else
        test_fail "bug_scanner config not found in config.sh"
    fi
}

# Test: Config has enabled option
test_config_enabled_option() {
    test_start "bug_scanner config has enabled option"
    if grep -q '"enabled"' "$PROJECT_ROOT/src/lib/config.sh"; then
        test_pass
    else
        test_fail "enabled option not found in bug_scanner config"
    fi
}

# Test: Config has severity_threshold option
test_config_severity_option() {
    test_start "bug_scanner config has severity_threshold option"
    if grep -q '"severity_threshold"' "$PROJECT_ROOT/src/lib/config.sh"; then
        test_pass
    else
        test_fail "severity_threshold option not found in bug_scanner config"
    fi
}

# Test: Config has preflight_check option
test_config_preflight_option() {
    test_start "bug_scanner config has preflight_check option"
    if grep -q '"preflight_check"' "$PROJECT_ROOT/src/lib/config.sh"; then
        test_pass
    else
        test_fail "preflight_check option not found in bug_scanner config"
    fi
}

# Test: Runner loop sources bug_scanner
test_runner_sources_scanner() {
    test_start "runner/loop.sh sources bug_scanner module"
    if grep -q 'source.*bug_scanner.sh' "$PROJECT_ROOT/src/runner/loop.sh"; then
        test_pass
    else
        test_fail "bug_scanner.sh not sourced in loop.sh"
    fi
}

# Test: Agent dispatch sources bug_scanner
test_dispatch_sources_scanner() {
    test_start "agent/dispatch.sh sources bug_scanner module"
    if grep -q 'source.*bug_scanner.sh' "$PROJECT_ROOT/src/agent/dispatch.sh"; then
        test_pass
    else
        test_fail "bug_scanner.sh not sourced in dispatch.sh"
    fi
}

# Test: Runner loop integrates bug scanner
test_runner_integrates_scanner() {
    test_start "runner/loop.sh integrates bug scanner in _needle_complete_bead"
    if grep -q 'bug_scanner_scan_bead' "$PROJECT_ROOT/src/runner/loop.sh"; then
        test_pass
    else
        test_fail "bug_scanner_scan_bead not called in loop.sh"
    fi
}

# Test: Agent dispatch integrates preflight check
test_dispatch_integrates_preflight() {
    test_start "agent/dispatch.sh integrates preflight check"
    if grep -q 'bug_scanner_quick_check' "$PROJECT_ROOT/src/agent/dispatch.sh"; then
        test_pass
    else
        test_fail "bug_scanner_quick_check not called in dispatch.sh"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Bug Scanner Module Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_module_exists
test_module_sourceable
test_quality_directory_exists
test_bug_scanner_available_exists
test_severity_level_exists
test_meets_threshold_exists
test_scan_bead_exists
test_quick_check_exists
test_status_exists
test_severity_level_critical
test_severity_level_error
test_severity_level_warning
test_severity_level_info
test_severity_level_case_insensitive
test_meets_threshold_critical_error
test_meets_threshold_warning_error
test_meets_threshold_info_info
test_meets_threshold_error_warning
test_status_returns_json
test_direct_run_help
test_direct_run_status
test_module_variables_set
test_default_enabled
test_default_severity
test_default_fail_on_issues
test_default_timeout
test_sources_output
test_available_returns_false
test_install_ubs_exists
test_install_ubs_dispatch
test_check_ubs_defined
test_check_ubs_version
test_config_bug_scanner
test_config_enabled_option
test_config_severity_option
test_config_preflight_option
test_runner_sources_scanner
test_dispatch_sources_scanner
test_runner_integrates_scanner
test_dispatch_integrates_preflight

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
