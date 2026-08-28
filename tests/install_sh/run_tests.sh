#!/bin/bash
#
# Test runner for install.sh checksum verification tests
#
# Usage:
#   ./run_tests.sh [OPTIONS]
#
# Options:
#   -v, --verbose    Run with verbose output
#   -f, --filter     Run tests matching pattern
#   -c, --count      Show test count only
#   -h, --help       Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_FILE="${SCRIPT_DIR}/checksum_verification.bats"

# Default options
BATS_OPTS=()

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        -v|--verbose)
            BATS_OPTS+=("-t")
            shift
            ;;
        -f|--filter)
            if [[ -n "${2:-}" ]]; then
                BATS_OPTS+=("-f" "$2")
                shift 2
            else
                echo "Error: --filter requires a pattern" >&2
                exit 1
            fi
            ;;
        -c|--count)
            BATS_OPTS+=("-c")
            shift
            ;;
        -h|--help)
            cat <<EOF
Usage: ./run_tests.sh [OPTIONS]

Run install.sh checksum verification tests.

Options:
    -v, --verbose    Run with verbose output
    -f, --filter     Run tests matching pattern
    -c, --count      Show test count only
    -h, --help       Show this help message

Examples:
    ./run_tests.sh                    # Run all tests
    ./run_tests.sh -v                 # Run with verbose output
    ./run_tests.sh -f "checksum"      # Run tests matching "checksum"
    ./run_tests.sh -c                 # Show test count only

Environment:
    BATS_BIN    Path to bats binary (default: auto-detect)
EOF
            exit 0
            ;;
        *)
            echo "Error: Unknown option: $1" >&2
            echo "Use -h for help" >&2
            exit 1
            ;;
    esac
done

# Check if Bats is available
BATS_BIN="${BATS_BIN:-bats}"
if ! command -v "$BATS_BIN" &>/dev/null; then
    cat <<EOF
Error: Bats (Bash Automated Testing System) is not installed.

Install Bats:
  Ubuntu/Debian: sudo apt install bats
  macOS:         brew install bats-core

Or download from: https://github.com/bats-core/bats-core
EOF
    exit 1
fi

# Check if test file exists
if [[ ! -f "$TEST_FILE" ]]; then
    echo "Error: Test file not found: $TEST_FILE" >&2
    exit 1
fi

# Run tests
echo "Running install.sh checksum verification tests..."
echo "Test file: $TEST_FILE"
echo "Bats binary: $BATS_BIN"
echo ""

cd "$SCRIPT_DIR"
exec "$BATS_BIN" "${BATS_OPTS[@]}" "$TEST_FILE"
