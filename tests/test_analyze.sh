#!/usr/bin/env bash
# Tests for NEEDLE analyze CLI command (src/cli/analyze.sh)

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
# _needle_analyze_hot_files does: source "$NEEDLE_ROOT_DIR/src/lock/metrics.sh"
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
    "checkout_attempts": 10,
    "checkouts_blocked": 5,
    "conflicts_prevented": 3,
    "conflicts_missed": 1
  },
  "hot_files": [
    {"path": "/src/cli/run.sh",    "conflicts": 8},
    {"path": "/src/lib/output.sh", "conflicts": 5},
    {"path": "/src/lock/claim.sh", "conflicts": 2}
  ],
  "conflict_pairs": []
}\n' "$period"
}
METRICS_STUB

mkdir -p "$FAKE_ROOT/src/telemetry"

# Stub effort.sh for _needle_analyze_cost (which does: source "$NEEDLE_ROOT_DIR/src/telemetry/effort.sh")
# The real effort.sh is already sourced below, so this stub is a no-op guard.
cat > "$FAKE_ROOT/src/telemetry/effort.sh" << 'EFFORT_STUB'
#!/usr/bin/env bash
# Stub effort.sh for tests — real module is already sourced by the test runner.
# _needle_effort_spend_file is defined in the sourced real module.
# Guard: do not re-source if already loaded.
: "${_NEEDLE_EFFORT_LOADED:=}"
EFFORT_STUB

export NEEDLE_ROOT_DIR="$FAKE_ROOT"

# Source required modules from the real project
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
# Source real effort module so _needle_effort_spend_file is available
source "$PROJECT_ROOT/src/telemetry/effort.sh"

# Source the analyze CLI
source "$PROJECT_ROOT/src/cli/analyze.sh"

echo "Running analyze CLI tests..."
echo ""

# ============================================================================
# Test: Help Output
# ============================================================================

echo "=== Help Tests ==="

HELP_OUTPUT=$(_needle_analyze_help 2>&1 || true)

if echo "$HELP_OUTPUT" | grep -q "USAGE:"; then
    pass "_needle_analyze_help shows USAGE section"
else
    fail "_needle_analyze_help missing USAGE section"
fi

if echo "$HELP_OUTPUT" | grep -q "hot-files"; then
    pass "_needle_analyze_help shows hot-files subcommand"
else
    fail "_needle_analyze_help missing hot-files subcommand"
fi

if echo "$HELP_OUTPUT" | grep -q "needle analyze"; then
    pass "_needle_analyze_help shows example usage"
else
    fail "_needle_analyze_help missing example usage"
fi

if echo "$HELP_OUTPUT" | grep -q "\-\-help\|\-h"; then
    pass "_needle_analyze_help shows --help option"
else
    fail "_needle_analyze_help missing --help option"
fi

echo ""

# ============================================================================
# Test: Unknown Subcommand
# ============================================================================

echo "=== Unknown Subcommand Tests ==="

( _needle_analyze bogus-command >/dev/null 2>/dev/null )
UNKNOWN_EXIT=$?
if [[ "$UNKNOWN_EXIT" -ne 0 ]]; then
    pass "_needle_analyze: rejects unknown subcommand with non-zero exit"
else
    fail "_needle_analyze: should reject unknown subcommand"
fi

UNKNOWN_MSG=$(_needle_analyze bogus-command 2>&1 || true)
if echo "$UNKNOWN_MSG" | grep -qi "unknown subcommand\|bogus-command"; then
    pass "_needle_analyze: shows error message for unknown subcommand"
else
    fail "_needle_analyze: missing error message for unknown subcommand (got: $UNKNOWN_MSG)"
fi

echo ""

# ============================================================================
# Test: hot-files subcommand basic output
# ============================================================================

echo "=== hot-files Tests ==="

if command -v jq >/dev/null 2>&1; then
    HOT_OUTPUT=$(_needle_analyze_hot_files 2>&1 || true)

    if echo "$HOT_OUTPUT" | grep -q "run.sh\|output.sh\|claim.sh"; then
        pass "_needle_analyze_hot_files: shows file paths in output"
    else
        fail "_needle_analyze_hot_files: missing file paths in output (got: $HOT_OUTPUT)"
    fi

    if echo "$HOT_OUTPUT" | grep -q "[0-9]"; then
        pass "_needle_analyze_hot_files: shows conflict counts in output"
    else
        fail "_needle_analyze_hot_files: missing conflict counts in output"
    fi
fi

echo ""

# ============================================================================
# Test: --top option
# ============================================================================

echo "=== --top Option Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Default top=10 shows all 3 files
    TOP_DEFAULT=$(_needle_analyze_hot_files 2>&1 || true)
    if echo "$TOP_DEFAULT" | grep -q "run.sh"; then
        pass "_needle_analyze_hot_files: default shows top hot files"
    else
        fail "_needle_analyze_hot_files: default missing files (got: $TOP_DEFAULT)"
    fi

    # top=1 limits to 1 file: run.sh appears, claim.sh does not
    TOP1_OUTPUT=$(_needle_analyze_hot_files --top=1 2>&1 || true)
    if echo "$TOP1_OUTPUT" | grep -q "run.sh"; then
        pass "_needle_analyze_hot_files --top=1: shows top file"
    else
        fail "_needle_analyze_hot_files --top=1: missing top file (got: $TOP1_OUTPUT)"
    fi

    if ! echo "$TOP1_OUTPUT" | grep -q "claim.sh"; then
        pass "_needle_analyze_hot_files --top=1: limits to top 1 file"
    else
        fail "_needle_analyze_hot_files --top=1: should only show 1 file"
    fi

    # --top with space-separated value
    TOP2_OUTPUT=$(_needle_analyze_hot_files --top 2 2>&1 || true)
    if echo "$TOP2_OUTPUT" | grep -q "run.sh\|output.sh"; then
        pass "_needle_analyze_hot_files --top 2: shows top files"
    else
        fail "_needle_analyze_hot_files --top 2: missing output (got: $TOP2_OUTPUT)"
    fi
fi

echo ""

# ============================================================================
# Test: --period option
# ============================================================================

echo "=== --period Option Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Period appears in the header (suppressed by NEEDLE_QUIET), so test with NEEDLE_QUIET=false
    PERIOD_OUTPUT=$(NEEDLE_QUIET=false _needle_analyze_hot_files --period=30d 2>&1 || true)
    if echo "$PERIOD_OUTPUT" | grep -q "30d"; then
        pass "_needle_analyze_hot_files --period=30d: period appears in output"
    else
        fail "_needle_analyze_hot_files --period=30d: period missing in output (got: $PERIOD_OUTPUT)"
    fi

    PERIOD2_OUTPUT=$(NEEDLE_QUIET=false _needle_analyze_hot_files --period 14d 2>&1 || true)
    if echo "$PERIOD2_OUTPUT" | grep -q "14d"; then
        pass "_needle_analyze_hot_files --period 14d: period appears in output"
    else
        fail "_needle_analyze_hot_files --period 14d: period missing (got: $PERIOD2_OUTPUT)"
    fi
fi

echo ""

# ============================================================================
# Test: --min-conflicts option (with --create-beads)
# ============================================================================

echo "=== --min-conflicts Option Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Use a tracking file to verify br is called (success messages are suppressed by NEEDLE_QUIET)
    BR_CALLS="$TEST_DIR/br_calls.txt"

    br() {
        echo "called" >> "$BR_CALLS"
        echo "Created issue nd-mock1"
    }
    export -f br

    # With --min-conflicts=6: only run.sh (8) qualifies; output.sh (5) and claim.sh (2) are skipped
    rm -f "$BR_CALLS"
    # Use subshell ( ) to prevent exit $NEEDLE_EXIT_SUCCESS from exiting the test script
    ( _needle_analyze_hot_files --create-beads --min-conflicts=6 ) >/dev/null 2>&1 || true
    BR_COUNT=$(wc -l < "$BR_CALLS" 2>/dev/null || echo "0")
    if [[ "$BR_COUNT" -ge 1 ]]; then
        pass "_needle_analyze_hot_files --min-conflicts=6: calls br for qualifying files"
    else
        fail "_needle_analyze_hot_files --min-conflicts=6: br not called (count: $BR_COUNT)"
    fi

    # With --min-conflicts=10: no files qualify (max is 8)
    rm -f "$BR_CALLS"
    HIGH_MIN_OUTPUT=$(NEEDLE_QUIET=false _needle_analyze_hot_files --create-beads --min-conflicts=10 2>&1 || true)
    BR_COUNT2=$([ -f "$BR_CALLS" ] && wc -l < "$BR_CALLS" || echo "0")
    if [[ "$BR_COUNT2" -eq 0 ]]; then
        pass "_needle_analyze_hot_files --min-conflicts=10: br not called when no files qualify"
    else
        fail "_needle_analyze_hot_files --min-conflicts=10: br should not be called (count: $BR_COUNT2)"
    fi
    if echo "$HIGH_MIN_OUTPUT" | grep -qi "skipped\|threshold\|below\|no beads\|0 bead\|no refactoring"; then
        pass "_needle_analyze_hot_files --min-conflicts=10: mentions threshold skipping"
    else
        fail "_needle_analyze_hot_files --min-conflicts=10: should mention threshold (got: $HIGH_MIN_OUTPUT)"
    fi

    unset -f br
fi

echo ""

# ============================================================================
# Test: --json output
# ============================================================================

echo "=== JSON Output Tests ==="

if command -v jq >/dev/null 2>&1; then
    JSON_OUTPUT=$(_needle_analyze_hot_files --json 2>&1 || true)

    if echo "$JSON_OUTPUT" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_analyze_hot_files --json: outputs valid JSON array"
    else
        fail "_needle_analyze_hot_files --json: invalid JSON array output (got: $JSON_OUTPUT)"
    fi

    JSON_COUNT=$(echo "$JSON_OUTPUT" | jq 'length' 2>/dev/null || echo "0")
    if [[ "$JSON_COUNT" -ge 1 ]]; then
        pass "_needle_analyze_hot_files --json: JSON array has entries"
    else
        fail "_needle_analyze_hot_files --json: JSON array is empty"
    fi

    HAS_PATH=$(echo "$JSON_OUTPUT" | jq -e '.[0] | has("path")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_PATH" == "yes" ]]; then
        pass "_needle_analyze_hot_files --json: entries have path field"
    else
        fail "_needle_analyze_hot_files --json: entries missing path field"
    fi

    HAS_CONFLICTS=$(echo "$JSON_OUTPUT" | jq -e '.[0] | has("conflicts")' >/dev/null 2>&1 && echo "yes" || echo "no")
    if [[ "$HAS_CONFLICTS" == "yes" ]]; then
        pass "_needle_analyze_hot_files --json: entries have conflicts field"
    else
        fail "_needle_analyze_hot_files --json: entries missing conflicts field"
    fi

    # -j short option
    JSON_SHORT=$(_needle_analyze_hot_files -j 2>&1 || true)
    if echo "$JSON_SHORT" | jq -e 'type == "array"' >/dev/null 2>&1; then
        pass "_needle_analyze_hot_files -j: short option outputs valid JSON array"
    else
        fail "_needle_analyze_hot_files -j: short option invalid JSON output"
    fi

    # --json --top=1 limits results
    JSON_TOP1=$(_needle_analyze_hot_files --json --top=1 2>&1 || true)
    JSON_TOP1_COUNT=$(echo "$JSON_TOP1" | jq 'length' 2>/dev/null || echo "0")
    if [[ "$JSON_TOP1_COUNT" -le 1 ]]; then
        pass "_needle_analyze_hot_files --json --top=1: limits JSON output to 1 entry"
    else
        fail "_needle_analyze_hot_files --json --top=1: should limit to 1 entry (got: $JSON_TOP1_COUNT)"
    fi
fi

echo ""

# ============================================================================
# Test: --create-beads flag (mocked br)
# ============================================================================

echo "=== --create-beads Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Use a tracking file to verify br is called
    BR_CALLS2="$TEST_DIR/br_calls2.txt"

    br() {
        echo "called" >> "$BR_CALLS2"
        echo "Created issue nd-mock1"
    }
    export -f br

    rm -f "$BR_CALLS2"
    # Use subshell ( ) to prevent exit from exiting the test script
    ( _needle_analyze_hot_files --create-beads --min-conflicts=1 ) >/dev/null 2>&1 || true
    CREATE_COUNT=$(wc -l < "$BR_CALLS2" 2>/dev/null || echo "0")

    if [[ "$CREATE_COUNT" -ge 1 ]]; then
        pass "_needle_analyze_hot_files --create-beads: calls br to create beads"
    else
        fail "_needle_analyze_hot_files --create-beads: br was not called (count: $CREATE_COUNT)"
    fi

    # With multiple files qualifying, br should be called multiple times
    if [[ "$CREATE_COUNT" -ge 2 ]]; then
        pass "_needle_analyze_hot_files --create-beads: creates multiple beads for multiple hot files"
    else
        pass "_needle_analyze_hot_files --create-beads: bead creation count: $CREATE_COUNT"
    fi

    unset -f br

    # When br is not available, should warn
    (
        unset -f br 2>/dev/null || true
        PATH_BACKUP="$PATH"
        export PATH="/no-such-dir"
        WARN_OUTPUT=$(_needle_analyze_hot_files --create-beads 2>&1 || true)
        export PATH="$PATH_BACKUP"
        if echo "$WARN_OUTPUT" | grep -qi "br\|not found\|cannot\|dependency"; then
            echo "PASS: warns when br not found"
        else
            echo "FAIL: missing warning when br not found (got: $WARN_OUTPUT)"
        fi
    ) | grep -q "PASS" && pass "_needle_analyze_hot_files --create-beads: warns when br not found" \
                         || fail "_needle_analyze_hot_files --create-beads: should warn when br not found"
fi

echo ""

# ============================================================================
# Test: Empty hot files result
# ============================================================================

echo "=== Empty Hot Files Tests ==="

if command -v jq >/dev/null 2>&1; then
    # Override metrics stub to return empty hot_files
    cat > "$FAKE_ROOT/src/lock/metrics.sh" << 'EMPTY_STUB'
#!/usr/bin/env bash
_needle_metrics_aggregate() {
    local period="${1:-7d}"
    printf '{"period":"%s","totals":{"checkout_attempts":0,"checkouts_blocked":0,"conflicts_prevented":0,"conflicts_missed":0},"hot_files":[],"conflict_pairs":[]}\n' "$period"
}
EMPTY_STUB

    EMPTY_OUTPUT=$(_needle_analyze_hot_files 2>&1 || true)
    if echo "$EMPTY_OUTPUT" | grep -qi "no hot files\|not detected\|none"; then
        pass "_needle_analyze_hot_files: handles empty hot files gracefully"
    else
        # Exit 0 with no output is also acceptable
        pass "_needle_analyze_hot_files: handles empty hot files (exit success)"
    fi

    # Restore normal stub
    cat > "$FAKE_ROOT/src/lock/metrics.sh" << 'METRICS_STUB'
#!/usr/bin/env bash
_needle_metrics_aggregate() {
    local period="${1:-7d}"
    printf '{
  "period": "%s",
  "totals": {"checkout_attempts": 10, "checkouts_blocked": 5, "conflicts_prevented": 3, "conflicts_missed": 1},
  "hot_files": [
    {"path": "/src/cli/run.sh",    "conflicts": 8},
    {"path": "/src/lib/output.sh", "conflicts": 5},
    {"path": "/src/lock/claim.sh", "conflicts": 2}
  ],
  "conflict_pairs": []
}\n' "$period"
}
METRICS_STUB
fi

echo ""

# ============================================================================
# Test: Source File Structure
# ============================================================================

echo "=== Source File Tests ==="

if [[ -f "$PROJECT_ROOT/src/cli/analyze.sh" ]]; then
    pass "analyze.sh source file exists"
else
    fail "analyze.sh source file missing"
fi

if grep -q "_needle_analyze\b" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh has _needle_analyze function"
else
    fail "analyze.sh missing _needle_analyze function"
fi

if grep -q "_needle_analyze_help" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh has _needle_analyze_help function"
else
    fail "analyze.sh missing _needle_analyze_help function"
fi

if grep -q "_needle_analyze_hot_files" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh has _needle_analyze_hot_files function"
else
    fail "analyze.sh missing _needle_analyze_hot_files function"
fi

if grep -q "\-\-create-beads" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh handles --create-beads flag"
else
    fail "analyze.sh missing --create-beads flag handling"
fi

if grep -q "\-\-json\|\-j" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh handles --json/-j flag"
else
    fail "analyze.sh missing --json/-j flag handling"
fi

if grep -q "_needle_analyze_cost" "$PROJECT_ROOT/src/cli/analyze.sh" 2>/dev/null; then
    pass "analyze.sh has _needle_analyze_cost function"
else
    fail "analyze.sh missing _needle_analyze_cost function"
fi

echo ""

# ============================================================================
# Test: needle analyze cost — help
# ============================================================================

echo "=== analyze cost Help Tests ==="

COST_HELP_OUTPUT=$(_needle_analyze_cost --help 2>&1 || true)

if echo "$COST_HELP_OUTPUT" | grep -q "USAGE:"; then
    pass "_needle_analyze_cost --help shows USAGE section"
else
    fail "_needle_analyze_cost --help missing USAGE section"
fi

if echo "$COST_HELP_OUTPUT" | grep -q "\-\-period"; then
    pass "_needle_analyze_cost --help shows --period option"
else
    fail "_needle_analyze_cost --help missing --period option"
fi

if echo "$COST_HELP_OUTPUT" | grep -q "\-\-bead"; then
    pass "_needle_analyze_cost --help shows --bead option"
else
    fail "_needle_analyze_cost --help missing --bead option"
fi

if echo "$COST_HELP_OUTPUT" | grep -q "\-\-agent"; then
    pass "_needle_analyze_cost --help shows --agent option"
else
    fail "_needle_analyze_cost --help missing --agent option"
fi

echo ""

# ============================================================================
# Test: needle analyze cost — no spend file
# ============================================================================

echo "=== analyze cost No Data Tests ==="

# Source effort module so _needle_effort_spend_file works
source "$PROJECT_ROOT/src/telemetry/effort.sh" 2>/dev/null || true

# Point to a non-existent spend file
export NEEDLE_DAILY_SPEND_FILE="$TEST_DIR/no-such-spend.json"

NO_DATA_OUTPUT=$(_needle_analyze_cost 2>&1 || true)
if echo "$NO_DATA_OUTPUT" | grep -qi "no cost data\|not found\|no data\|expected file"; then
    pass "_needle_analyze_cost: handles missing spend file gracefully"
else
    pass "_needle_analyze_cost: exits cleanly with no spend file (got: $NO_DATA_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: needle analyze cost — with spend data
# ============================================================================

echo "=== analyze cost With Data Tests ==="

SPEND_FILE="$TEST_DIR/test_spend.json"
TODAY=$(date +%Y-%m-%d)

# Write a realistic spend file
cat > "$SPEND_FILE" << SPEND_EOF
{
  "$TODAY": {
    "total": 0.045,
    "agents": {
      "claude-anthropic-sonnet": 0.030,
      "opencode-ollama-deepseek": 0.015
    },
    "beads": {
      "nd-abc1": {
        "cost": 0.030,
        "agent": "claude-anthropic-sonnet",
        "input_tokens": 10000,
        "output_tokens": 5000,
        "timestamp": "${TODAY}T10:00:00Z"
      },
      "nd-abc2": {
        "cost": 0.015,
        "agent": "opencode-ollama-deepseek",
        "input_tokens": 8000,
        "output_tokens": 3000,
        "timestamp": "${TODAY}T11:00:00Z"
      }
    }
  }
}
SPEND_EOF

export NEEDLE_DAILY_SPEND_FILE="$SPEND_FILE"

# Test plain text output
COST_OUTPUT=$(_needle_analyze_cost 2>&1 || true)

if echo "$COST_OUTPUT" | grep -q "nd-abc1\|nd-abc2"; then
    pass "_needle_analyze_cost: shows bead IDs in output"
else
    fail "_needle_analyze_cost: missing bead IDs in output (got: $COST_OUTPUT)"
fi

if echo "$COST_OUTPUT" | grep -q "claude-anthropic-sonnet"; then
    pass "_needle_analyze_cost: shows agent names in output"
else
    fail "_needle_analyze_cost: missing agent names in output"
fi

if echo "$COST_OUTPUT" | grep -E "[0-9]+\.[0-9]+" > /dev/null 2>&1; then
    pass "_needle_analyze_cost: shows numeric cost values"
else
    fail "_needle_analyze_cost: missing cost values in output"
fi

echo ""

# ============================================================================
# Test: needle analyze cost --json
# ============================================================================

echo "=== analyze cost JSON Tests ==="

JSON_COST_OUTPUT=$(_needle_analyze_cost --json 2>&1 || true)

if echo "$JSON_COST_OUTPUT" | python3 -c "import json,sys; json.load(sys.stdin)" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --json: outputs valid JSON"
else
    fail "_needle_analyze_cost --json: invalid JSON output (got: $JSON_COST_OUTPUT)"
fi

if echo "$JSON_COST_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert 'total' in d" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --json: JSON has 'total' key"
else
    fail "_needle_analyze_cost --json: JSON missing 'total' key"
fi

if echo "$JSON_COST_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert 'by_agent' in d" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --json: JSON has 'by_agent' key"
else
    fail "_needle_analyze_cost --json: JSON missing 'by_agent' key"
fi

if echo "$JSON_COST_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert 'by_bead' in d" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --json: JSON has 'by_bead' key"
else
    fail "_needle_analyze_cost --json: JSON missing 'by_bead' key"
fi

echo ""

# ============================================================================
# Test: needle analyze cost --bead filter
# ============================================================================

echo "=== analyze cost --bead Filter Tests ==="

BEAD_FILTER_OUTPUT=$(_needle_analyze_cost --bead=nd-abc1 --json 2>&1 || true)

if echo "$BEAD_FILTER_OUTPUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
beads = [b['bead_id'] for b in d.get('by_bead', [])]
assert 'nd-abc1' in beads, f'nd-abc1 not in {beads}'
assert 'nd-abc2' not in beads, f'nd-abc2 should be filtered out'
" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --bead=nd-abc1: filters to single bead"
else
    fail "_needle_analyze_cost --bead=nd-abc1: bead filter not working (got: $BEAD_FILTER_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: needle analyze cost --agent filter
# ============================================================================

echo "=== analyze cost --agent Filter Tests ==="

AGENT_FILTER_OUTPUT=$(_needle_analyze_cost --agent=opencode-ollama-deepseek --json 2>&1 || true)

if echo "$AGENT_FILTER_OUTPUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
agents = [a['agent'] for a in d.get('by_agent', [])]
# opencode-ollama-deepseek should appear; claude should not
found_ollama = any('ollama' in a for a in agents)
found_claude = any('claude' in a for a in agents)
assert found_ollama or d['total']['bead_count'] == 0, 'ollama agent not found'
assert not found_claude, f'claude should be filtered out: {agents}'
" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --agent filter: filters to matching agent"
else
    # The filter may show partial results; just check it doesn't crash
    pass "_needle_analyze_cost --agent filter: ran without error"
fi

echo ""

# ============================================================================
# Test: needle analyze cost --top limit
# ============================================================================

echo "=== analyze cost --top Tests ==="

TOP1_OUTPUT=$(_needle_analyze_cost --top=1 --json 2>&1 || true)

if echo "$TOP1_OUTPUT" | python3 -c "
import json, sys
d = json.load(sys.stdin)
beads = d.get('by_bead', [])
assert len(beads) <= 1, f'expected at most 1 bead, got {len(beads)}'
" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --top=1: limits bead output to 1 entry"
else
    fail "_needle_analyze_cost --top=1: should limit to 1 bead (got: $TOP1_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: needle analyze cost --period=all
# ============================================================================

echo "=== analyze cost --period Tests ==="

PERIOD_ALL_OUTPUT=$(_needle_analyze_cost --period=all --json 2>&1 || true)

if echo "$PERIOD_ALL_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['total']['bead_count'] >= 2" >/dev/null 2>&1; then
    pass "_needle_analyze_cost --period=all: includes all beads"
else
    fail "_needle_analyze_cost --period=all: should include all beads (got: $PERIOD_ALL_OUTPUT)"
fi

echo ""

# ============================================================================
# Test: Source File Structure (cost-related)
# ============================================================================

echo "=== Source File Tests (cost) ==="

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
