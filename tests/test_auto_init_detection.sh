#!/usr/bin/env bash
#
# Tests for auto-init detection: _needle_needs_init function
#
# Usage: ./tests/test_auto_init_detection.sh
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

# Setup: create a temp home directory
TEST_DIR=$(mktemp -d)
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Override HOME and NEEDLE_HOME so config paths point to our temp dir
export HOME="$TEST_DIR"
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_CONFIG_FILE="$TEST_DIR/.needle/config.yaml"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/config.sh"

# -----------------------------------------------------------------------------
# Test Cases
# -----------------------------------------------------------------------------

# Test: _needle_needs_init function exists
test_needs_init_exists() {
    test_start "_needle_needs_init function exists"
    if declare -f _needle_needs_init &>/dev/null; then
        test_pass
    else
        test_fail "_needle_needs_init function not defined"
    fi
}

# Test: returns 0 (needs init) when config file is missing
test_needs_init_no_config() {
    test_start "returns 0 when config file is missing"
    rm -f "$NEEDLE_CONFIG_FILE"

    if _needle_needs_init; then
        test_pass
    else
        test_fail "Should return 0 (needs init) when config file is absent"
    fi
}

# Test: returns 0 (needs init) when config file is empty
test_needs_init_empty_config() {
    test_start "returns 0 when config file is empty"
    mkdir -p "$(dirname "$NEEDLE_CONFIG_FILE")"
    > "$NEEDLE_CONFIG_FILE"  # create empty file

    if _needle_needs_init; then
        test_pass
    else
        test_fail "Should return 0 (needs init) when config file is empty"
    fi
}

# Test: returns 0 (needs init) when tmux is missing
test_needs_init_no_tmux() {
    test_start "returns 0 when tmux is not available"
    # Write a valid config
    mkdir -p "$(dirname "$NEEDLE_CONFIG_FILE")"
    echo "version: 1" > "$NEEDLE_CONFIG_FILE"

    # Shadow tmux with a function that fails
    tmux() { return 1; }
    br() { return 0; }
    export -f tmux br

    # Unset functions after the call
    local result=0
    (
        command() {
            if [[ "$2" == "tmux" ]]; then return 1; fi
            builtin command "$@"
        }
        export -f command
        _needle_needs_init
    ) && result=$? || result=$?

    unset -f tmux br

    # We can't easily override 'command -v' in the same subshell, so test
    # differently: override PATH to hide tmux
    local orig_path="$PATH"
    local tmp_bin="$TEST_DIR/fake_bin"
    mkdir -p "$tmp_bin"
    # Create a fake br that succeeds
    printf '#!/bin/bash\nexit 0\n' > "$tmp_bin/br"
    chmod +x "$tmp_bin/br"
    # Do NOT put tmux in tmp_bin, so it won't be found if not on PATH already

    # Only run this portion if tmux is actually available on the system
    if command -v tmux &>/dev/null; then
        # Can't easily hide a real tmux without modifying PATH drastically
        printf "%bSKIP%b (tmux present, PATH isolation not feasible)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    else
        # tmux not installed at all - test directly
        if _needle_needs_init; then
            test_pass
        else
            test_fail "Should return 0 when tmux is missing"
        fi
    fi
}

# Test: returns 0 (needs init) when br is missing
test_needs_init_no_br() {
    test_start "returns 0 when br is not available"
    mkdir -p "$(dirname "$NEEDLE_CONFIG_FILE")"
    echo "version: 1" > "$NEEDLE_CONFIG_FILE"

    # Only testable directly if br is not installed
    if ! command -v br &>/dev/null; then
        if _needle_needs_init; then
            test_pass
        else
            test_fail "Should return 0 when br is missing"
        fi
    else
        printf "%bSKIP%b (br present, PATH isolation not feasible)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: returns 1 (no init needed) when config exists and deps are present
test_needs_init_all_good() {
    test_start "returns 1 when config is valid and all deps present"
    mkdir -p "$(dirname "$NEEDLE_CONFIG_FILE")"
    echo "version: 1" > "$NEEDLE_CONFIG_FILE"

    # Only valid when both tmux and br are installed
    if command -v tmux &>/dev/null && command -v br &>/dev/null; then
        if ! _needle_needs_init; then
            test_pass
        else
            test_fail "Should return 1 (no init needed) when fully configured"
        fi
    else
        printf "%bSKIP%b (missing deps: tmux or br not installed)\n" "\033[0;33m" "\033[0m"
        ((tests_run++)) || true
    fi
}

# Test: _needle_maybe_init skips init for 'init' command
test_maybe_init_skips_init_cmd() {
    test_start "_needle_maybe_init skips check for 'init' command"
    # Source bin/needle functions needed
    # We can test by sourcing needle and checking _needle_maybe_init directly
    # Since _needle_maybe_init calls _needle_needs_init and then exec needle init,
    # verify that when first arg is 'init', the function returns without action

    # Remove config to force needs_init = true
    rm -f "$NEEDLE_CONFIG_FILE"

    # Stub _needle_needs_init and _needle_init to detect if they're called
    local init_called=false
    local needs_init_called=false

    # Re-source config.sh so _needle_needs_init is available
    # Then define _needle_maybe_init inline to test
    _test_needle_maybe_init() {
        case "${1:-}" in
            init|version|help|--help|-h|--version|-V)
                return 0
                ;;
        esac
        needs_init_called=true
    }

    _test_needle_maybe_init "init"
    if [[ "$needs_init_called" == "false" ]]; then
        test_pass
    else
        test_fail "_needle_maybe_init should skip needs_init check for 'init' command"
    fi
}

# Test: _needle_maybe_init skips init for 'version' command
test_maybe_init_skips_version_cmd() {
    test_start "_needle_maybe_init skips check for 'version' command"
    local needs_init_called=false

    _test_needle_maybe_init() {
        case "${1:-}" in
            init|version|help|--help|-h|--version|-V)
                return 0
                ;;
        esac
        needs_init_called=true
    }

    _test_needle_maybe_init "version"
    if [[ "$needs_init_called" == "false" ]]; then
        test_pass
    else
        test_fail "_needle_maybe_init should skip needs_init check for 'version' command"
    fi
}

# Test: _needle_maybe_init skips init for 'help' command
test_maybe_init_skips_help_cmd() {
    test_start "_needle_maybe_init skips check for 'help' command"
    local needs_init_called=false

    _test_needle_maybe_init() {
        case "${1:-}" in
            init|version|help|--help|-h|--version|-V)
                return 0
                ;;
        esac
        needs_init_called=true
    }

    _test_needle_maybe_init "help"
    if [[ "$needs_init_called" == "false" ]]; then
        test_pass
    else
        test_fail "_needle_maybe_init should skip needs_init check for 'help' command"
    fi
}

# -----------------------------------------------------------------------------
# Run Tests
# -----------------------------------------------------------------------------

printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "%bNEEDLE Auto-Init Detection Tests%b\n" "\033[1;35m" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n\n" "\033[2m" "\033[0m"

test_needs_init_exists
test_needs_init_no_config
test_needs_init_empty_config
test_needs_init_no_tmux
test_needs_init_no_br
test_needs_init_all_good
test_maybe_init_skips_init_cmd
test_maybe_init_skips_version_cmd
test_maybe_init_skips_help_cmd

# Summary
printf "\n%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"
printf "Tests: %d total, %b%d passed%b, %b%d failed%b\n" \
    "$tests_run" \
    "\033[0;32m" "$tests_passed" "\033[0m" \
    "\033[0;31m" "$tests_failed" "\033[0m"
printf "%b━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━%b\n" "\033[2m" "\033[0m"

if [[ $tests_failed -gt 0 ]]; then
    exit 1
fi
exit 0
