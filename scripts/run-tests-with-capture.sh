#!/usr/bin/env bash
#
# run-tests-with-capture.sh
#
# Test runner script for bead-forge that captures all cargo test output
# (stdout/stderr) to timestamped trace files in .beads/traces/
#
# Usage:
#   ./run-tests-with-capture.sh [cargo-test-args...]
#
# Examples:
#   ./run-tests-with-capture.sh                    # Run all tests
#   ./run-tests-with-capture.sh --lib              # Run library tests only
#   ./run-tests-with-capture.sh test::test_name    # Run specific test
#   ./run-tests-with-capture.sh -- --nocapture     # Pass args to test binary

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to log messages
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*" >&2
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*" >&2
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $*" >&2
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

# Determine paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_DIR="$(dirname "$SCRIPT_DIR")"

# Try multiple possible bead-forge locations
BEAD_FORGE_DIR="${HOME}/bead-forge"
if [[ ! -d "$BEAD_FORGE_DIR" ]]; then
    BEAD_FORGE_DIR="${NEEDLE_DIR}/bead-forge"
fi
if [[ ! -d "$BEAD_FORGE_DIR" ]]; then
    BEAD_FORGE_DIR="$(pwd)"  # Current directory might be bead-forge
fi

BEADS_DIR="${BEAD_FORGE_DIR}/.beads"
TRACES_DIR="${BEADS_DIR}/traces"

# Ensure we're in the correct directory
if [[ ! -d "$BEAD_FORGE_DIR" ]]; then
    log_error "bead-forge directory not found at: $BEAD_FORGE_DIR"
    exit 1
fi

# Ensure traces directory exists
mkdir -p "$TRACES_DIR"
log_info "Traces directory: $TRACES_DIR"

# Generate timestamp for trace file
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
TRACE_FILE="${TRACES_DIR}/cargo-test-${TIMESTAMP}.log"

log_info "Trace file: $TRACE_FILE"
log_info "Starting cargo test run..."

# Change to bead-forge directory
cd "$BEAD_FORGE_DIR"

# Capture cargo test output (both stdout and stderr) to trace file
# Also display to console in real-time
if cargo test "$@" 2>&1 | tee "$TRACE_FILE"; then
    TEST_RESULT=0
    log_success "Tests completed successfully"
else
    TEST_RESULT=$?
    log_error "Tests failed with exit code: $TEST_RESULT"
fi

# Generate summary
log_info "Generating test summary..."

# Extract test count and results if available
if grep -q "test result:" "$TRACE_FILE"; then
    TEST_SUMMARY=$(grep "test result:" "$TRACE_FILE" | tail -1)
    log_info "Test summary: $TEST_SUMMARY"
fi

# Extract warning count if any
WARNING_COUNT=$(grep -c "^warning:" "$TRACE_FILE" 2>/dev/null || echo "0")
if [[ "$WARNING_COUNT" -gt 0 ]]; then
    log_warning "Found $WARNING_COUNT compiler warnings"
fi

# Create symlink to latest trace for easy access
LATEST_LINK="${TRACES_DIR}/cargo-test-latest.log"
ln -sf "$(basename "$TRACE_FILE")" "$LATEST_LINK"
log_info "Latest trace symlink: $LATEST_LINK"

# Report file size
FILE_SIZE=$(du -h "$TRACE_FILE" | cut -f1)
log_info "Trace file size: $FILE_SIZE"

# Exit with cargo test's exit code
exit $TEST_RESULT