#!/usr/bin/env bash
# Tests for NEEDLE refactor CLI command (src/cli/refactor.sh)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
    printf "${GREEN}PASS${NC} %s\n" "$1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

fail() {
    printf "${RED}FAIL${NC} %s\n" "$1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# ============================================================================
# Set up test environment
# ============================================================================

TEST_DIR=$(mktemp -d)

cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_QUIET=true
export NEEDLE_USE_COLOR=false
mkdir -p "$NEEDLE_HOME"

# Create a fake NEEDLE_ROOT_DIR with a stub metrics.sh.
# _needle_refactor_suggest does: source "$NEEDLE_ROOT_DIR/src/lock/metrics.sh"
# We intercept this by providing our own stub that defines _needle_metrics_aggregate.
FAKE_ROOT="$TEST_DIR/fake-needle"
mkdir -p "$FAKE_ROOT/src/lock"

cat > "$FAKE_ROOT/src/lock/metrics.sh" << 'METRICS_STUB'
#!/usr/bin/env bash
# Stub metrics.sh for tests
_needle_metrics_aggregate() {
    local period="${1:-7d}"
    printf '{
  "period": "%s",
  "totals": {
    "checkout_attempts": 15,
    "checkouts_blocked": 7,
    "conflicts_prevented": 4,
    "conflicts_missed": 2
  },
  "hot_files": [
    {"path": "/src/cli/run.sh",    "conflicts": 12},
    {"path": "/src/lib/output.sh", "conflicts": 6}
  ],
  "conflict_pairs": []
}\n' "$period"
}
METRICS_STUB

export NEEDLE_ROOT_DIR="$FAKE_ROOT"

# Source required modules from the real project
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"

# Create a test file with known size
TEST_FILE="$TEST_DIR/test_file.sh"
cat > "$TEST_FILE" << 'EOF'
#!/usr/bin/env bash
# Test file for refactor suggest tests
echo "hello world"
EOF

# Create a large test file (>500 lines) for size-based suggestions
LARGE_FILE="$TEST_DIR/large_file.sh"
{
    echo "#!/usr/bin/env bash"
    for i in $(seq 1 510); do
        echo "# line $i"
    done
} > "$LARGE_FILE"

# A Python file for language-specific tests
PY_FILE="$TEST_DIR/module.py"
echo "# Python test file" > "$PY_FILE"

# Source the refactor CLI (after mocking metrics)
source "$PROJECT_ROOT/src/cli/refactor.sh"

echo "Running refactor CLI tests..."
echo ""

# ============================================================================
# Test: Help Output
# ============================================================================

echo "=== Help Tests ==="

HELP_OUTPUT=$(_needle_refactor_help 2>&1 || true)

if echo "$HELP_OUTPUT" | grep -q "USAGE:"; then
    pass "_needle_refactor_help shows USAGE section"
else
    fail "_needle_refactor_help missing USAGE section"
fi

if echo "$HELP_OUTPUT" | grep -q "suggest"; then
    pass "_needle_refactor_help shows suggest subcommand"
else
    fail "_needle_refactor_help missing suggest subcommand"
fi

if echo "$HELP_OUTPUT" | grep -q "needle refactor"; then
    pass "_needle_refactor_help shows example usage"
else
    fail "_needle_refactor_help missing example usage"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-help\|\-h"; then
    pass "_needle_refactor_help shows --help option"
else
    fail "_needle_refactor_help missing --help option"
fi

echo ""

# ============================================================================
# Test: Unknown Subcommand
# ============================================================================

echo "=== Unknown Subcommand Tests ==="

( _needle_refactor bogus-command >/dev/null 2>/dev/null )
UNKNOWN_EXIT=$?
if [[ "$UNKNOWN_EXIT" -ne 0 ]]; then
    pass "_needle_refactor: rejects unknown subcommand with non-zero exit"
else
    fail "_needle_refactor: should reject unknown subcommand"
fi

UNKNOWN_MSG=$(_needle_refactor bogus-command 2>&1 || true)
if echo "$UNKNOWN_MSG" | grep -qi "unknown subcommand\|bogus-command"; then
    pass "_needle_refactor: shows error message for unknown subcommand"
else
    fail "_needle_refactor: missing error message for unknown subcommand (got: $UNKNOWN_MSG)"
fi

echo ""

# ============================================================================
# Test: Missing filepath error
# ============================================================================

echo "=== Missing Filepath Tests ==="

( _needle_refactor_suggest >/dev/null 2>/dev/null )
NO_FILE_EXIT=$?
if [[ "$NO_FILE_EXIT" -ne 0 ]]; then
    pass "_needle_refactor_suggest: exits non-zero when filepath missing"
else
    fail "_needle_refactor_suggest: should exit non-zero with no filepath"
fi

NO_FILE_MSG=$(_needle_refactor_suggest 2>&1 || true)
if echo "$NO_FILE_MSG" | grep -qi "usage\|file\|suggest"; then
    pass "_needle_refactor_suggest: shows usage error when filepath missing"
else
    fail "_needle_refactor_suggest: missing error message when no filepath (got: $NO_FILE_MSG)"
fi

# Dispatch through _needle_refactor suggest with no file
( _needle_refactor suggest >/dev/null 2>/dev/null )
NO_FILE_VIA_CMD_EXIT=$?
if [[ "$NO_FILE_VIA_CMD_EXIT" -ne 0 ]]; then
    pass "_needle_refactor suggest: exits non-zero when filepath missing"
else
    fail "_needle_refactor suggest: should exit non-zero with no filepath"
fi

echo ""

# ============================================================================
# Test: suggest subcommand basic output
# ============================================================================

echo "=== suggest Subcommand Tests ==="

if command -v jq >/dev/null 2>&1; then
    SUGGEST_OUTPUT=$(_needle_refactor_suggest "$TEST_FILE" 2>&1 || true)

    if echo "$SUGGEST_OUTPUT" | grep -qi "suggestion\|refactor\|split\|module"; then
        pass "_needle_refactor_suggest: shows refactoring suggestions"
    else
        fail "_needle_refactor_suggest: missing refactoring suggestions (got: $SUGGEST_OUTPUT)"
    fi

    if echo "$SUGGEST_OUTPUT" | grep -q "$(basename "$TEST_FILE")"; then
        pass "_needle_refactor_suggest: shows filename in output"
    else
        fail "_needle_refactor_suggest: missing filename in output"
    fi

    if echo "$SUGGEST_OUTPUT" | grep -q "7d"; then
        pass "_needle_refactor_suggest: shows default period in output"
    else
        fail "_needle_refactor_suggest: missing period in output"
    fi
fi

echo ""

# ============================================================================
# Test: --period option
# ============================================================================

echo "=== --period Option Tests ==="

if command -v jq >/dev/null 2>&1; then
    PERIOD_OUTPUT=$(_needle_refactor_suggest "$TEST_FILE" --period=30d 2>&1 || true)
    if echo "$PERIOD_OUTPUT" | grep -q "30d"; then
        pass "_needle_refactor_suggest --period=30d: period appears in output"
    else
        fail "_needle_refactor_suggest --period=30d: period missing (got: $PERIOD_OUTPUT)"
    fi

    PERIOD2_OUTPUT=$(_needle_refactor_suggest "$TEST_FILE" --period 14d 2>&1 || true)
    if echo "$PERIOD2_OUTPUT" | grep -q "14d"; then
        pass "_needle_refactor_suggest --period 14d: period appears in output"
    else
        fail "_needle_refactor_suggest --period 14d: period missing (got: $PERIOD2_OUTPUT)"
    fi
fi

echo ""

# ============================================================================
# Test: --json output
# ============================================================================

echo "=== JSON Output Tests ==="

if command -v jq >/dev/null 2>&1; then
    JSON_OUTPUT=$(_needle_refactor_suggest "$TEST_FILE" --json 2>&1 || true)

    if echo "$JSON_OUTPUT" | jq -e 'type == "object"' >/dev/null 2>&1; then
        pass "_needle_refactor_suggest --json: outputs valid JSON object"
    else
        fail "_needle_refactor_suggest --json: invalid JSON output (got: $JSON_OUTPUT)"
    fi

    HAS_FILE=$(echo "$JSON_OUTPUT" | jq -e 'has("file")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_FILE" == "yes" ]]; then
        pass "_needle_refactor_suggest --json: output has file field"
    else
        fail "_needle_refactor_suggest --json: output missing file field"
    fi

    HAS_SUGGESTIONS=$(echo "$JSON_OUTPUT" | jq -e 'has("suggestions")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_SUGGESTIONS" == "yes" ]]; then
        pass "_needle_refactor_suggest --json: output has suggestions field"
    else
        fail "_needle_refactor_suggest --json: output missing suggestions field"
    fi

    HAS_CONFLICTS=$(echo "$JSON_OUTPUT" | jq -e 'has("conflicts")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_CONFLICTS" == "yes" ]]; then
        pass "_needle_refactor_suggest --json: output has conflicts field"
    else
        fail "_needle_refactor_suggest --json: output missing conflicts field"
    fi

    HAS_PERIOD=$(echo "$JSON_OUTPUT" | jq -e 'has("period")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_PERIOD" == "yes" ]]; then
        pass "_needle_refactor_suggest --json: output has period field"
    else
        fail "_needle_refactor_suggest --json: output missing period field"
    fi

    SUGG_COUNT=$(echo "$JSON_OUTPUT" | jq '.suggestions | length' 2>/dev/null || echo "0")
    if [[ "$SUGG_COUNT" -ge 1 ]]; then
        pass "_needle_refactor_suggest --json: suggestions array has entries"
    else
        fail "_needle_refactor_suggest --json: suggestions array is empty"
    fi

    # -j short option
    JSON_SHORT=$(_needle_refactor_suggest "$TEST_FILE" -j 2>&1 || true)
    if echo "$JSON_SHORT" | jq -e 'type == "object"' >/dev/null 2>&1; then
        pass "_needle_refactor_suggest -j: short option outputs valid JSON"
    else
        fail "_needle_refactor_suggest -j: short option invalid JSON output"
    fi

    # --json with custom period
    JSON_PERIOD=$(_needle_refactor_suggest "$TEST_FILE" --json --period=14d 2>&1 || true)
    JSON_PERIOD_VAL=$(echo "$JSON_PERIOD" | jq -r '.period' 2>/dev/null || echo "")
    if [[ "$JSON_PERIOD_VAL" == "14d" ]]; then
        pass "_needle_refactor_suggest --json --period=14d: period in JSON output"
    else
        fail "_needle_refactor_suggest --json --period=14d: wrong period in JSON (got: $JSON_PERIOD_VAL)"
    fi
fi

echo ""

# ============================================================================
# Test: File type-specific suggestions (shell)
# ============================================================================

echo "=== File Type-Specific Suggestion Tests ==="

if command -v jq >/dev/null 2>&1; then
    SH_JSON=$(_needle_refactor_suggest "$TEST_FILE" --json 2>&1 || true)
    SH_SUGGESTIONS=$(echo "$SH_JSON" | jq -r '.suggestions[]' 2>/dev/null || echo "")

    if echo "$SH_SUGGESTIONS" | grep -qi "module\|script\|lib\|source\|split"; then
        pass "_needle_refactor_suggest: gives shell-specific suggestions for .sh files"
    else
        fail "_needle_refactor_suggest: missing shell-specific suggestions (got: $SH_SUGGESTIONS)"
    fi

    # Python file
    PY_JSON=$(_needle_refactor_suggest "$PY_FILE" --json 2>&1 || true)
    PY_SUGGESTIONS=$(echo "$PY_JSON" | jq -r '.suggestions[]' 2>/dev/null || echo "")

    if echo "$PY_SUGGESTIONS" | grep -qi "package\|module\|class\|python\|import\|__init__"; then
        pass "_needle_refactor_suggest: gives Python-specific suggestions for .py files"
    else
        fail "_needle_refactor_suggest: missing Python-specific suggestions (got: $PY_SUGGESTIONS)"
    fi

    # Large file size suggestion
    LARGE_JSON=$(_needle_refactor_suggest "$LARGE_FILE" --json 2>&1 || true)
    LARGE_SUGGESTIONS=$(echo "$LARGE_JSON" | jq -r '.suggestions[]' 2>/dev/null || echo "")

    if echo "$LARGE_SUGGESTIONS" | grep -qi "large\|lines\|size\|split"; then
        pass "_needle_refactor_suggest: gives size-based suggestion for large files"
    else
        fail "_needle_refactor_suggest: missing size-based suggestion for large file (got: $LARGE_SUGGESTIONS)"
    fi
fi

echo ""

# ============================================================================
# Test: High-contention file advice
# ============================================================================

echo "=== Contention-Based Advice Tests ==="

if command -v jq >/dev/null 2>&1; then
    # run.sh has 12 conflicts in our mock (>10 threshold)
    HOT_FILE_PATH="/src/cli/run.sh"
    HOT_JSON=$(_needle_refactor_suggest "$HOT_FILE_PATH" --json 2>&1 || true)
    HOT_SUGGESTIONS=$(echo "$HOT_JSON" | jq -r '.suggestions[]' 2>/dev/null || echo "")

    if echo "$HOT_SUGGESTIONS" | grep -qi "conflict\|critical\|high\|contention\|hot spot"; then
        pass "_needle_refactor_suggest: shows high-contention advice for hot files"
    else
        fail "_needle_refactor_suggest: missing contention advice for hot files (got: $HOT_SUGGESTIONS)"
    fi

    HOT_CONFLICTS=$(echo "$HOT_JSON" | jq -r '.conflicts' 2>/dev/null || echo "0")
    if [[ "$HOT_CONFLICTS" -eq 12 ]]; then
        pass "_needle_refactor_suggest: reports correct conflict count for hot file"
    else
        fail "_needle_refactor_suggest: wrong conflict count (expected 12, got: $HOT_CONFLICTS)"
    fi

    # File with no conflicts
    NO_CONFLICT_JSON=$(_needle_refactor_suggest "$TEST_FILE" --json 2>&1 || true)
    NO_CONFLICT_SUGGESTIONS=$(echo "$NO_CONFLICT_JSON" | jq -r '.suggestions[]' 2>/dev/null || echo "")

    if echo "$NO_CONFLICT_SUGGESTIONS" | grep -qi "no recorded\|no conflict\|0 conflict"; then
        pass "_needle_refactor_suggest: reports no conflicts for clean file"
    else
        fail "_needle_refactor_suggest: missing no-conflict message (got: $NO_CONFLICT_SUGGESTIONS)"
    fi
fi

echo ""

# ============================================================================
# Test: Unknown option error
# ============================================================================

echo "=== Unknown Option Tests ==="

( _needle_refactor_suggest --unknown-opt "$TEST_FILE" >/dev/null 2>/dev/null )
UNKNOWN_OPT_EXIT=$?
if [[ "$UNKNOWN_OPT_EXIT" -ne 0 ]]; then
    pass "_needle_refactor_suggest: rejects unknown option with non-zero exit"
else
    fail "_needle_refactor_suggest: should reject unknown option"
fi

UNKNOWN_OPT_MSG=$(_needle_refactor_suggest --unknown-opt "$TEST_FILE" 2>&1 || true)
if echo "$UNKNOWN_OPT_MSG" | grep -qi "unknown option\|unknown-opt"; then
    pass "_needle_refactor_suggest: shows error for unknown option"
else
    fail "_needle_refactor_suggest: missing error for unknown option (got: $UNKNOWN_OPT_MSG)"
fi

echo ""

# ============================================================================
# Test: Source File Structure
# ============================================================================

echo "=== Source File Tests ==="

if [[ -f "$PROJECT_ROOT/src/cli/refactor.sh" ]]; then
    pass "refactor.sh source file exists"
else
    fail "refactor.sh source file missing"
fi

if grep -q "_needle_refactor\b" "$PROJECT_ROOT/src/cli/refactor.sh" 2>/dev/null; then
    pass "refactor.sh has _needle_refactor function"
else
    fail "refactor.sh missing _needle_refactor function"
fi

if grep -q "_needle_refactor_help" "$PROJECT_ROOT/src/cli/refactor.sh" 2>/dev/null; then
    pass "refactor.sh has _needle_refactor_help function"
else
    fail "refactor.sh missing _needle_refactor_help function"
fi

if grep -q "_needle_refactor_suggest" "$PROJECT_ROOT/src/cli/refactor.sh" 2>/dev/null; then
    pass "refactor.sh has _needle_refactor_suggest function"
else
    fail "refactor.sh missing _needle_refactor_suggest function"
fi

if grep -q "\-\-json\|\-j" "$PROJECT_ROOT/src/cli/refactor.sh" 2>/dev/null; then
    pass "refactor.sh handles --json/-j flag"
else
    fail "refactor.sh missing --json/-j flag handling"
fi

if grep -q "\-\-period" "$PROJECT_ROOT/src/cli/refactor.sh" 2>/dev/null; then
    pass "refactor.sh handles --period flag"
else
    fail "refactor.sh missing --period flag handling"
fi

echo ""

# ============================================================================
# Summary
# ============================================================================

echo "=== Summary ==="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -eq 0 ]]; then
    printf "${GREEN}All tests passed!${NC}\n"
    exit 0
else
    printf "${RED}Some tests failed!${NC}\n"
    exit 1
fi
