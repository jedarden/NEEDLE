#!/usr/bin/env bash
#
# Tests for bootstrap/detect_os.sh module
#
# Usage: ./tests/test_detect_os.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
DETECT_OS_MODULE="$PROJECT_ROOT/bootstrap/detect_os.sh"

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
    source "$DETECT_OS_MODULE"
}

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: Module file exists
test_module_exists() {
    test_start "Module file exists"
    if [[ -f "$DETECT_OS_MODULE" ]]; then
        test_pass
    else
        test_fail "bootstrap/detect_os.sh not found"
    fi
}

# Test: Module is sourceable
test_module_sourceable() {
    test_start "Module is sourceable"
    if source "$DETECT_OS_MODULE" 2>/dev/null; then
        test_pass
    else
        test_fail "Failed to source module"
    fi
}

# Test: detect_os function exists
test_detect_os_exists() {
    test_start "detect_os function exists"
    source_module
    if declare -f detect_os &>/dev/null; then
        test_pass
    else
        test_fail "detect_os function not defined"
    fi
}

# Test: detect_os returns valid value
test_detect_os_valid() {
    test_start "detect_os returns valid value"
    source_module
    local result
    result=$(detect_os)
    case "$result" in
        linux|macos|windows|wsl|unknown)
            test_pass
            ;;
        *)
            test_fail "detect_os returned invalid value: $result"
            ;;
    esac
}

# Test: detect_os correctly identifies current system
test_detect_os_current() {
    test_start "detect_os correctly identifies current system"
    source_module
    local result
    result=$(detect_os)
    local expected

    case "$(uname -s)" in
        Linux*)
            if [[ -f /proc/version ]] && grep -qi microsoft /proc/version 2>/dev/null; then
                expected="wsl"
            else
                expected="linux"
            fi
            ;;
        Darwin*)
            expected="macos"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            expected="windows"
            ;;
        *)
            expected="unknown"
            ;;
    esac

    if [[ "$result" == "$expected" ]]; then
        test_pass
    else
        test_fail "Expected '$expected', got '$result'"
    fi
}

# Test: detect_distro function exists
test_detect_distro_exists() {
    test_start "detect_distro function exists"
    source_module
    if declare -f detect_distro &>/dev/null; then
        test_pass
    else
        test_fail "detect_distro function not defined"
    fi
}

# Test: detect_distro returns valid value
test_detect_distro_valid() {
    test_start "detect_distro returns valid value"
    source_module
    local os
    os=$(detect_os)

    if [[ "$os" != "linux" && "$os" != "wsl" ]]; then
        # On non-Linux, should return unknown
        local result
        result=$(detect_distro)
        if [[ "$result" == "unknown" ]]; then
            test_pass
        else
            test_fail "Non-Linux system should return 'unknown', got '$result'"
        fi
        return
    fi

    local result
    result=$(detect_distro)
    case "$result" in
        debian|fedora|arch|suse|alpine|unknown)
            test_pass
            ;;
        *)
            test_fail "detect_distro returned invalid value: $result"
            ;;
    esac
}

# Test: detect_distro_name function exists
test_detect_distro_name_exists() {
    test_start "detect_distro_name function exists"
    source_module
    if declare -f detect_distro_name &>/dev/null; then
        test_pass
    else
        test_fail "detect_distro_name function not defined"
    fi
}

# Test: detect_distro_version function exists
test_detect_distro_version_exists() {
    test_start "detect_distro_version function exists"
    source_module
    if declare -f detect_distro_version &>/dev/null; then
        test_pass
    else
        test_fail "detect_distro_version function not defined"
    fi
}

# Test: detect_pkg_manager function exists
test_detect_pkg_manager_exists() {
    test_start "detect_pkg_manager function exists"
    source_module
    if declare -f detect_pkg_manager &>/dev/null; then
        test_pass
    else
        test_fail "detect_pkg_manager function not defined"
    fi
}

# Test: detect_pkg_manager returns valid value
test_detect_pkg_manager_valid() {
    test_start "detect_pkg_manager returns valid value"
    source_module
    local result
    result=$(detect_pkg_manager)
    case "$result" in
        brew|apt|dnf|yum|pacman|zypper|apk|chocolatey|scoop|manual)
            test_pass
            ;;
        *)
            test_fail "detect_pkg_manager returned invalid value: $result"
            ;;
    esac
}

# Test: detect_pkg_manager detects available package manager
test_detect_pkg_manager_available() {
    test_start "detect_pkg_manager finds available manager"
    source_module
    local result
    result=$(detect_pkg_manager)

    if [[ "$result" != "manual" ]]; then
        test_pass
    else
        # Check if we really have no package manager (edge case)
        if ! command -v apt-get &>/dev/null && \
           ! command -v dnf &>/dev/null && \
           ! command -v yum &>/dev/null && \
           ! command -v pacman &>/dev/null && \
           ! command -v brew &>/dev/null && \
           ! command -v apk &>/dev/null; then
            test_pass  # Correctly identified no package manager
        else
            test_fail "Package manager exists but not detected"
        fi
    fi
}

# Test: get_install_command function exists
test_get_install_command_exists() {
    test_start "get_install_command function exists"
    source_module
    if declare -f get_install_command &>/dev/null; then
        test_pass
    else
        test_fail "get_install_command function not defined"
    fi
}

# Test: get_install_command returns correct commands
test_get_install_command_values() {
    test_start "get_install_command returns correct values"
    source_module

    local cmd
    cmd=$(get_install_command "apt")
    if [[ "$cmd" != "apt-get install -y" ]]; then
        test_fail "apt install command incorrect: $cmd"
        return
    fi

    cmd=$(get_install_command "brew")
    if [[ "$cmd" != "brew install" ]]; then
        test_fail "brew install command incorrect: $cmd"
        return
    fi

    cmd=$(get_install_command "pacman")
    if [[ "$cmd" != "pacman -S --noconfirm" ]]; then
        test_fail "pacman install command incorrect: $cmd"
        return
    fi

    test_pass
}

# Test: get_update_command function exists
test_get_update_command_exists() {
    test_start "get_update_command function exists"
    source_module
    if declare -f get_update_command &>/dev/null; then
        test_pass
    else
        test_fail "get_update_command function not defined"
    fi
}

# Test: get_update_command returns correct commands
test_get_update_command_values() {
    test_start "get_update_command returns correct values"
    source_module

    local cmd
    cmd=$(get_update_command "apt")
    if [[ "$cmd" != "apt-get update" ]]; then
        test_fail "apt update command incorrect: $cmd"
        return
    fi

    cmd=$(get_update_command "brew")
    if [[ "$cmd" != "brew update" ]]; then
        test_fail "brew update command incorrect: $cmd"
        return
    fi

    test_pass
}

# Test: detect_arch function exists
test_detect_arch_exists() {
    test_start "detect_arch function exists"
    source_module
    if declare -f detect_arch &>/dev/null; then
        test_pass
    else
        test_fail "detect_arch function not defined"
    fi
}

# Test: detect_arch returns valid value
test_detect_arch_valid() {
    test_start "detect_arch returns valid value"
    source_module
    local result
    result=$(detect_arch)
    case "$result" in
        amd64|arm64|armv7|armv6|i386|unknown)
            test_pass
            ;;
        *)
            test_fail "detect_arch returned invalid value: $result"
            ;;
    esac
}

# Test: detect_arch correctly identifies current architecture
test_detect_arch_current() {
    test_start "detect_arch correctly identifies current architecture"
    source_module
    local result
    result=$(detect_arch)
    local expected

    case "$(uname -m)" in
        x86_64|amd64)
            expected="amd64"
            ;;
        aarch64|arm64)
            expected="arm64"
            ;;
        armv7l|armhf)
            expected="armv7"
            ;;
        armv6l)
            expected="armv6"
            ;;
        i386|i686)
            expected="i386"
            ;;
        *)
            expected="unknown"
            ;;
    esac

    if [[ "$result" == "$expected" ]]; then
        test_pass
    else
        test_fail "Expected '$expected', got '$result'"
    fi
}

# Test: detect_arch_go function exists
test_detect_arch_go_exists() {
    test_start "detect_arch_go function exists"
    source_module
    if declare -f detect_arch_go &>/dev/null; then
        test_pass
    else
        test_fail "detect_arch_go function not defined"
    fi
}

# Test: detect_arch_rust function exists
test_detect_arch_rust_exists() {
    test_start "detect_arch_rust function exists"
    source_module
    if declare -f detect_arch_rust &>/dev/null; then
        test_pass
    else
        test_fail "detect_arch_rust function not defined"
    fi
}

# Test: pkg_is_installed function exists
test_pkg_is_installed_exists() {
    test_start "pkg_is_installed function exists"
    source_module
    if declare -f pkg_is_installed &>/dev/null; then
        test_pass
    else
        test_fail "pkg_is_installed function not defined"
    fi
}

# Test: get_system_info function exists
test_get_system_info_exists() {
    test_start "get_system_info function exists"
    source_module
    if declare -f get_system_info &>/dev/null; then
        test_pass
    else
        test_fail "get_system_info function not defined"
    fi
}

# Test: get_system_info outputs expected fields
test_get_system_info_output() {
    test_start "get_system_info outputs expected fields"
    source_module
    local output
    output=$(get_system_info)

    if echo "$output" | grep -q "OS:" && \
       echo "$output" | grep -q "Distro:" && \
       echo "$output" | grep -q "Package Mgr:" && \
       echo "$output" | grep -q "Architecture:"; then
        test_pass
    else
        test_fail "get_system_info missing expected fields"
    fi
}

# Test: get_system_info_json function exists
test_get_system_info_json_exists() {
    test_start "get_system_info_json function exists"
    source_module
    if declare -f get_system_info_json &>/dev/null; then
        test_pass
    else
        test_fail "get_system_info_json function not defined"
    fi
}

# Test: get_system_info_json outputs valid JSON
test_get_system_info_json_valid() {
    test_start "get_system_info_json outputs valid JSON"
    source_module
    local output
    output=$(get_system_info_json)

    # Basic JSON validation - check for expected structure
    if echo "$output" | grep -qE '^\{"os":' && \
       echo "$output" | grep -q '"distro":' && \
       echo "$output" | grep -q '"arch":'; then
        test_pass
    else
        test_fail "Invalid JSON output: $output"
    fi
}

# Test: export_system_info function exists
test_export_system_info_exists() {
    test_start "export_system_info function exists"
    source_module
    if declare -f export_system_info &>/dev/null; then
        test_pass
    else
        test_fail "export_system_info function not defined"
    fi
}

# Test: export_system_info sets environment variables
test_export_system_info_vars() {
    test_start "export_system_info sets environment variables"
    source_module
    export_system_info

    if [[ -n "${NEEDLE_OS:-}" ]] && \
       [[ -n "${NEEDLE_DISTRO:-}" ]] && \
       [[ -n "${NEEDLE_PKG_MANAGER:-}" ]] && \
       [[ -n "${NEEDLE_ARCH:-}" ]]; then
        test_pass
    else
        test_fail "Environment variables not set correctly"
    fi
}

# Test: is_root function exists
test_is_root_exists() {
    test_start "is_root function exists"
    source_module
    if declare -f is_root &>/dev/null; then
        test_pass
    else
        test_fail "is_root function not defined"
    fi
}

# Test: needs_sudo function exists
test_needs_sudo_exists() {
    test_start "needs_sudo function exists"
    source_module
    if declare -f needs_sudo &>/dev/null; then
        test_pass
    else
        test_fail "needs_sudo function not defined"
    fi
}

# Test: is_supported_system function exists
test_is_supported_system_exists() {
    test_start "is_supported_system function exists"
    source_module
    if declare -f is_supported_system &>/dev/null; then
        test_pass
    else
        test_fail "is_supported_system function not defined"
    fi
}

# Test: is_supported_system returns correctly
test_is_supported_system_current() {
    test_start "is_supported_system identifies current system"
    source_module

    local os
    os=$(detect_os)

    if [[ "$os" == "linux" || "$os" == "macos" || "$os" == "wsl" ]]; then
        if is_supported_system; then
            test_pass
        else
            test_fail "Supported system reported as unsupported"
        fi
    else
        if ! is_supported_system; then
            test_pass
        else
            test_fail "Unsupported system reported as supported"
        fi
    fi
}

# Test: Module can be run directly with --help
test_direct_run_help() {
    test_start "Module runs directly with --help"
    local output
    output=$(bash "$DETECT_OS_MODULE" --help 2>&1)
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
    output=$(bash "$DETECT_OS_MODULE" --json 2>&1)
    if echo "$output" | grep -qE '^\{"os":'; then
        test_pass
    else
        test_fail "Direct run with --json failed: $output"
    fi
}

# Test: Module can be run directly without arguments
test_direct_run_default() {
    test_start "Module runs directly without arguments"
    local output
    output=$(bash "$DETECT_OS_MODULE" 2>&1)
    if echo "$output" | grep -q "System Information"; then
        test_pass
    else
        test_fail "Direct run without arguments failed"
    fi
}

# Test: get_sudo_prefix function exists
test_get_sudo_prefix_exists() {
    test_start "get_sudo_prefix function exists"
    source_module
    if declare -f get_sudo_prefix &>/dev/null; then
        test_pass
    else
        test_fail "get_sudo_prefix function not defined"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE OS Detection Module Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_module_exists
test_module_sourceable
test_detect_os_exists
test_detect_os_valid
test_detect_os_current
test_detect_distro_exists
test_detect_distro_valid
test_detect_distro_name_exists
test_detect_distro_version_exists
test_detect_pkg_manager_exists
test_detect_pkg_manager_valid
test_detect_pkg_manager_available
test_get_install_command_exists
test_get_install_command_values
test_get_update_command_exists
test_get_update_command_values
test_detect_arch_exists
test_detect_arch_valid
test_detect_arch_current
test_detect_arch_go_exists
test_detect_arch_rust_exists
test_pkg_is_installed_exists
test_get_system_info_exists
test_get_system_info_output
test_get_system_info_json_exists
test_get_system_info_json_valid
test_export_system_info_exists
test_export_system_info_vars
test_is_root_exists
test_needs_sudo_exists
test_is_supported_system_exists
test_is_supported_system_current
test_get_sudo_prefix_exists
test_direct_run_help
test_direct_run_json
test_direct_run_default

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
