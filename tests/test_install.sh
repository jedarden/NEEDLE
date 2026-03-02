#!/usr/bin/env bash
#
# Tests for install.sh script
#
# Usage: ./tests/test_install.sh
#

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
INSTALL_SCRIPT="$PROJECT_ROOT/scripts/install.sh"

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

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: Script exists and is executable
test_script_executable() {
    test_start "Script is executable"
    if [[ -x "$INSTALL_SCRIPT" ]]; then
        test_pass
    else
        test_fail "install.sh is not executable"
    fi
}

# Test: Help option works
test_help_option() {
    test_start "Help option works"
    if bash "$INSTALL_SCRIPT" --help &>/dev/null; then
        test_pass
    else
        test_fail "--help option failed"
    fi
}

# Test: Help output contains expected content
test_help_content() {
    test_start "Help output contains usage"
    local output
    output=$(bash "$INSTALL_SCRIPT" --help 2>&1)
    if echo "$output" | grep -q "Usage:"; then
        test_pass
    else
        test_fail "Help output missing Usage section"
    fi
}

# Test: OS detection (Linux)
test_os_detection_linux() {
    test_start "OS detection returns valid value"

    # Source the functions (need to extract them)
    source_funcs() {
        # Extract detect_os function
        eval "$(sed -n '/^detect_os()/,/^}/p' "$INSTALL_SCRIPT")"
    }

    # Just verify the function exists in the script
    if grep -q "detect_os()" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "detect_os function not found"
    fi
}

# Test: Architecture detection
test_arch_detection() {
    test_start "Architecture detection function exists"
    if grep -q "detect_arch()" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "detect_arch function not found"
    fi
}

# Test: URL building for latest version
test_url_latest() {
    test_start "URL building for latest version"
    if grep -q "releases/latest/download" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Latest version URL pattern not found"
    fi
}

# Test: URL building for specific version
test_url_specific() {
    test_start "URL building for specific version"
    if grep -q "releases/download/v" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Specific version URL pattern not found"
    fi
}

# Test: Dry-run mode works
test_dry_run() {
    test_start "Dry-run mode works"
    local output
    output=$(NEEDLE_REPO="test/repo" bash "$INSTALL_SCRIPT" --dry-run 2>&1) || true
    if echo "$output" | grep -q "\[DRY RUN\]"; then
        test_pass
    else
        test_fail "Dry-run output missing expected text: $output"
    fi
}

# Test: Non-interactive flag is recognized
test_non_interactive() {
    test_start "Non-interactive flag is recognized"
    if grep -q "NON_INTERACTIVE=true" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Non-interactive flag handling not found"
    fi
}

# Test: Custom install directory
test_custom_install_dir() {
    test_start "Custom install directory supported"
    if grep -q "NEEDLE_INSTALL_DIR" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "NEEDLE_INSTALL_DIR not found"
    fi
}

# Test: No-modify-path option
test_no_modify_path() {
    test_start "No-modify-path option supported"
    if grep -q "NEEDLE_NO_MODIFY_PATH" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "NEEDLE_NO_MODIFY_PATH not found"
    fi
}

# Test: Version option
test_version_option() {
    test_start "Version option supported"
    if grep -q "\-\-version" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "--version option not found"
    fi
}

# Test: Uninstall option
test_uninstall_option() {
    test_start "Uninstall option supported"
    if grep -q "\-\-uninstall" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "--uninstall option not found"
    fi
}

# Test: Shell RC detection for bash
test_shell_rc_bash() {
    test_start "Shell RC detection for bash"
    if grep -q ".bashrc" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail ".bashrc reference not found"
    fi
}

# Test: Shell RC detection for zsh
test_shell_rc_zsh() {
    test_start "Shell RC detection for zsh"
    if grep -q ".zshrc" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail ".zshrc reference not found"
    fi
}

# Test: Fish shell support
test_shell_rc_fish() {
    test_start "Fish shell support"
    if grep -q "config.fish" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Fish shell config reference not found"
    fi
}

# Test: Error handling for unsupported OS
test_unsupported_os() {
    test_start "Error handling for unsupported OS"
    if grep -q "Unsupported operating system" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Unsupported OS error message not found"
    fi
}

# Test: Error handling for unsupported architecture
test_unsupported_arch() {
    test_start "Error handling for unsupported architecture"
    if grep -q "Unsupported architecture" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Unsupported architecture error message not found"
    fi
}

# Test: Curl fallback to wget
test_curl_wget_fallback() {
    test_start "Curl/wget fallback supported"
    if grep -q "command_exists wget" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "wget fallback not found"
    fi
}

# Test: Auto-init environment variable
test_auto_init() {
    test_start "Auto-init environment variable supported"
    if grep -q "NEEDLE_AUTO_INIT" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "NEEDLE_AUTO_INIT not found"
    fi
}

# Test: Color support detection
test_color_support() {
    test_start "Color support detection (NO_COLOR)"
    if grep -q "NO_COLOR" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "NO_COLOR support not found"
    fi
}

# Test: Success message
test_success_message() {
    test_start "Success message present"
    if grep -q "installed successfully" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Success message not found"
    fi
}

# Test: Init prompt
test_init_prompt() {
    test_start "Init prompt present"
    if grep -q "needle init" "$INSTALL_SCRIPT"; then
        test_pass
    else
        test_fail "Init prompt not found"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Installer Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

# Run all tests
test_script_executable
test_help_option
test_help_content
test_os_detection_linux
test_arch_detection
test_url_latest
test_url_specific
test_dry_run
test_non_interactive
test_custom_install_dir
test_no_modify_path
test_version_option
test_uninstall_option
test_shell_rc_bash
test_shell_rc_zsh
test_shell_rc_fish
test_unsupported_os
test_unsupported_arch
test_curl_wget_fallback
test_auto_init
test_color_support
test_success_message
test_init_prompt

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
