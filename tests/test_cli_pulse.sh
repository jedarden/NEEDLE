#!/usr/bin/env bash
# Test suite for pulse CLI command (nd-3jns)
#
# Tests the needle pulse command for manual pulse scans.

set -uo pipefail

# Get test directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# Helper functions
pass() {
    echo -e "${GREEN}PASS${NC} $1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

fail() {
    echo -e "${RED}FAIL${NC} $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# Path to needle CLI
NEEDLE_CLI="$PROJECT_ROOT/bin/needle"

echo "Running pulse CLI tests..."
echo ""

# ============================================================================
# Test: Pulse CLI Source File
# ============================================================================

echo "=== CLI Source File Tests ==="

if [[ -f "$PROJECT_ROOT/src/cli/pulse.sh" ]]; then
    pass "Pulse CLI source file exists at src/cli/pulse.sh"
else
    fail "Pulse CLI source file missing"
fi

if grep -q "_needle_pulse\b" "$PROJECT_ROOT/src/cli/pulse.sh" 2>/dev/null; then
    pass "Pulse CLI has _needle_pulse function"
else
    fail "Pulse CLI missing _needle_pulse function"
fi

if grep -q "_needle_pulse_help" "$PROJECT_ROOT/src/cli/pulse.sh" 2>/dev/null; then
    pass "Pulse CLI has _needle_pulse_help function"
else
    fail "Pulse CLI missing _needle_pulse_help function"
fi

echo ""

# ============================================================================
# Test: Pulse Command Registration
# ============================================================================

echo "=== Command Registration Tests ==="

if grep -q 'source.*pulse\.sh' "$PROJECT_ROOT/bin/needle" 2>/dev/null; then
    pass "Pulse CLI is sourced in main needle script"
else
    fail "Pulse CLI not sourced in main needle script"
fi

if grep -q 'pulse)' "$PROJECT_ROOT/bin/needle" 2>/dev/null; then
    pass "Pulse command is registered in route_command"
else
    fail "Pulse command not registered in route_command"
fi

echo ""

# ============================================================================
# Test: Pulse Help
# ============================================================================

echo "=== Help Tests ==="

HELP_OUTPUT=$("$NEEDLE_CLI" pulse --help 2>&1 || true)

if echo "$HELP_OUTPUT" | grep -q "Run pulse scans manually"; then
    pass "Pulse --help shows description"
else
    fail "Pulse --help missing description"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-workspace"; then
    pass "Pulse help shows --workspace option"
else
    fail "Pulse help missing --workspace option"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-detectors"; then
    pass "Pulse help shows --detectors option"
else
    fail "Pulse help missing --detectors option"
fi

if echo "$HELP_OUTPUT" | grep -q "security"; then
    pass "Pulse help lists security detector"
else
    fail "Pulse help missing security detector"
fi

# Check main help includes pulse
MAIN_HELP=$("$NEEDLE_CLI" help 2>&1 || true)
if echo "$MAIN_HELP" | grep -q "pulse"; then
    pass "Main help includes pulse command"
else
    fail "Main help missing pulse command"
fi

echo ""

# ============================================================================
# Test: Pulse Reset
# ============================================================================

echo "=== Reset Tests ==="

RESET_OUTPUT=$("$NEEDLE_CLI" pulse --reset --json 2>&1 || true)
RESET_JSON=$(echo "$RESET_OUTPUT" | sed -n '/^{/,/^}/p' | head -10)

if echo "$RESET_JSON" | jq -e '.reset == true' >/dev/null 2>&1; then
    pass "Pulse --reset outputs JSON with reset=true"
else
    fail "Pulse --reset does not output correct JSON"
fi

echo ""

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=== Summary ==="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed!${NC}"
    exit 1
fi
