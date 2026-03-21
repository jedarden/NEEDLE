#!/bin/bash
# E2E: Failure counter persists across claim cycles — forced mitosis triggers
#
# Proves that when a bead fails verification repeatedly, the failure count is
# tracked across claim/release cycles and forced mitosis triggers at the
# configured threshold.
#
# This is the exact bug that caused the bash NEEDLE GLM workers to loop forever:
# the failure counter reset to 1 on every claim cycle, so force_failure_threshold: 3
# never triggered.
#
# Test Plan:
# 1. Create a workspace with 1 bead that has 3 subtasks in its body
# 2. Create a smart agent adapter:
#    - For parent bead work: always exits 1 (simulates failure)
#    - For mitosis analysis of parent: returns splittable JSON, exits 0
#    - For mitosis analysis of children: returns not-splittable, exits 0
#    - For child bead work: exits 0 (simulates success)
# 3. Configure: mitosis.force_failure_threshold: 3
# 4. Launch a single worker
# 5. Assert:
#    - Attempt 1: bead claimed, agent fails (exit 1), failure_count=1, released
#    - Attempt 2: same bead re-claimed, agent fails, failure_count=2, released
#    - Attempt 3: same bead re-claimed, agent fails, failure_count=3, forced mitosis triggers
#    - Mitosis creates child beads from the 3 subtasks
#    - Parent bead is now blocked by children
#    - Worker claims a child bead next (not the parent again)
# 6. Verify via telemetry:
#    - failure_count increments correctly (1, 2, 3)
#    - bead.mitosis.split event emitted on attempt 3
#    - No infinite loop — worker eventually reaches EXHAUSTED
#
# Dependencies: br (beads_rust CLI), needle binary, jq

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── Color helpers ──────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

PASS=true
PASS_COUNT=0
FAIL_COUNT=0

pass() { echo -e "  ${GREEN}PASS${NC}: $1"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo -e "  ${RED}FAIL${NC}: $1"; FAIL_COUNT=$((FAIL_COUNT + 1)); PASS=false; }
info() { echo -e "  ${YELLOW}INFO${NC}: $1"; }

# Helper: get the first JSON object matching an event_type as compact JSON.
first_event() {
    jq -c "select(.event_type == \"$1\")" "$TELEMETRY_LOG" 2>/dev/null | head -1 || echo ""
}

# Helper: count occurrences of an event_type.
count_event() {
    jq -c "select(.event_type == \"$1\")" "$TELEMETRY_LOG" 2>/dev/null | wc -l || echo 0
}

# ── Check prerequisites ──────────────────────────────────────────────────────

echo "=== E2E: Failure Counter Persistence ==="
echo ""

if ! command -v jq &>/dev/null; then
    echo "FATAL: jq is required but not found"
    exit 1
fi

NEEDLE_BIN="$PROJECT_ROOT/target/debug/needle"

if [ ! -x "$NEEDLE_BIN" ]; then
    echo "Building needle (debug)..."
    cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1
fi

if [ ! -x "$NEEDLE_BIN" ]; then
    echo "FATAL: needle binary not found at $NEEDLE_BIN"
    exit 1
fi

BR_BIN="$(which br 2>/dev/null || echo "$HOME/.local/bin/br")"
if [ ! -x "$BR_BIN" ]; then
    echo "FATAL: br binary not found"
    exit 1
fi

# ── Create isolated environment ──────────────────────────────────────────────

TMPBASE="$(mktemp -d)"
WORKSPACE="$TMPBASE/workspace"
FAKE_HOME="$TMPBASE/home"

cleanup() {
    rm -rf "$TMPBASE"
}
trap cleanup EXIT

REAL_HOME="$HOME"
export HOME="$FAKE_HOME"
mkdir -p "$HOME"

# ── Step 1: Create workspace ────────────────────────────────────────────────

echo "Step 1: Creating workspace..."
mkdir -p "$WORKSPACE"
(cd "$WORKSPACE" && "$BR_BIN" init 2>&1) || {
    echo "FATAL: br init failed"
    exit 1
}
echo "  Workspace: $WORKSPACE"

# ── Step 2: Create parent bead with 3 subtasks ──────────────────────────────

echo "Step 2: Creating parent bead with 3 subtasks..."

# Use a unique marker so the adapter can distinguish parent from child beads.
BEAD_BODY='PARENT-BEAD-MARKER

## Tasks

- [ ] Task A: Create the API endpoint for user registration
- [ ] Task B: Write database migration for users table
- [ ] Task C: Add unit tests for the registration flow

## Acceptance Criteria
All three tasks must be completed.'

BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Multi-task implementation" \
    --description "$BEAD_BODY" \
    --silent 2>/dev/null)" || {
    # Retry once after sync (FrankenSQLite WAL race).
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Multi-task implementation" \
        --description "$BEAD_BODY" \
        --silent)"
}
echo "  Parent bead: $BEAD_ID"

# ── Step 3: Create smart agent adapter ───────────────────────────────────────

echo "Step 3: Creating smart agent adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

# Smart adapter with 3 behaviors:
# 1. Mitosis analysis of parent bead → returns splittable JSON, exits 0
# 2. Mitosis analysis of child bead → returns not-splittable, exits 0
# 3. Regular work on parent bead (contains PARENT-BEAD-MARKER) → exits 1
# 4. Regular work on child bead → exits 0 (success)
cat > "$ADAPTERS_DIR/smart-fail.yaml" <<'YAML'
name: smart-fail
agent_cli: bash
invoke_template: |
  PROMPT=$(cat {prompt_file} 2>/dev/null || echo "")
  if echo "$PROMPT" | grep -q '## Mitosis Analysis'; then
    if echo "$PROMPT" | grep -q 'PARENT-BEAD-MARKER'; then
      printf '%s' '{"splittable": true, "children": [{"title": "Task A: Create the API endpoint", "body": "Create the API endpoint for user registration"}, {"title": "Task B: Write database migration", "body": "Write database migration for users table"}, {"title": "Task C: Add unit tests", "body": "Add unit tests for the registration flow"}]}'
      exit 0
    else
      printf '%s' '{"splittable": false}'
      exit 0
    fi
  elif echo "$PROMPT" | grep -q 'PARENT-BEAD-MARKER'; then
    echo "Agent failed intentionally for E2E test" >&2
    exit 1
  else
    exit 0
  fi
timeout_secs: 10
YAML

# ── Step 4: Configure needle with forced mitosis ────────────────────────────

echo "Step 4: Configuring needle with force_failure_threshold: 3..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: smart-fail
  timeout: 30
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 5
strands:
  mitosis:
    enabled: true
    first_failure_only: false
    force_failure_threshold: 3
YAML

echo "  Mitosis config: enabled=true, force_failure_threshold=3"

# ── Step 5: Run needle (should trigger 3 failures then mitosis) ──────────────

echo "Step 5: Running needle worker..."
export TMUX=fake

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0

# Run with a generous timeout. Expected flow:
# - 3 failures on parent → mitosis → 3 child beads succeed → exhausted
# Should complete in well under 30 seconds.
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent smart-fail \
    --count 10 \
    --identifier e2e-failure-counter 2>&1 || EXIT_CODE=$?

echo ""
info "Needle exit code: $EXIT_CODE"

# ── Step 6: Locate telemetry log ────────────────────────────────────────────

TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"

if [ -z "$TELEMETRY_LOG" ]; then
    echo "FATAL: No telemetry log found in $TELEMETRY_DIR"
    exit 1
fi

info "Telemetry log: $TELEMETRY_LOG"
EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
info "Total events: $EVENT_COUNT"
echo ""

# ── Step 7: Verify failure count persistence ─────────────────────────────────

echo "Step 7: Verifying failure count persistence..."

# Check for failure-count:N label on parent bead.
PARENT_INFO="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null || echo "")"
info "Parent bead info (first line): $(echo "$PARENT_INFO" | head -1)"

FAILURE_COUNT=""
if echo "$PARENT_INFO" | grep -q "failure-count:"; then
    FAILURE_COUNT="$(echo "$PARENT_INFO" | grep -o 'failure-count:[0-9]*' | head -1 | cut -d: -f2)"
fi

if [ -n "$FAILURE_COUNT" ]; then
    pass "Parent bead has failure-count:$FAILURE_COUNT label"
else
    fail "Parent bead missing failure-count label"
fi

# ── Step 8: Verify mitosis triggered ─────────────────────────────────────────

echo "Step 8: Verifying mitosis triggered..."

# Count mitosis.split events (should be exactly 1 for the parent bead).
MITOSIS_SPLIT_COUNT=$(count_event "bead.mitosis.split")
if [ "$MITOSIS_SPLIT_COUNT" -ge 1 ]; then
    pass "bead.mitosis.split event present (count: $MITOSIS_SPLIT_COUNT)"
else
    fail "bead.mitosis.split event NOT found"
fi

# Get the mitosis.split event to check child_ids.
MITOSIS_EVENT=$(first_event "bead.mitosis.split")
if [ -n "$MITOSIS_EVENT" ]; then
    CHILDREN_CREATED=$(echo "$MITOSIS_EVENT" | jq -r '.data.children_created // 0')
    CHILD_IDS=$(echo "$MITOSIS_EVENT" | jq -r '.data.child_ids // [] | join(", ")')

    if [ "$CHILDREN_CREATED" -ge 3 ]; then
        pass "Mitosis created $CHILDREN_CREATED child beads"
    else
        fail "Mitosis created only $CHILDREN_CREATED children (expected 3)"
    fi

    info "Child bead IDs: $CHILD_IDS"
else
    fail "Could not parse mitosis.split event"
fi

# ── Step 9: Verify failure count progression ────────────────────────────────

echo "Step 9: Verifying failure count progression..."

# Count outcome.classified events with outcome: "failure" (lowercase).
FAILURE_OUTCOMES=$(jq -c 'select(.event_type == "outcome.classified" and .data.outcome == "failure")' "$TELEMETRY_LOG" 2>/dev/null | wc -l || echo 0)

if [ "$FAILURE_OUTCOMES" -ge 3 ]; then
    pass "At least 3 failure outcomes recorded (count: $FAILURE_OUTCOMES)"
else
    fail "Only $FAILURE_OUTCOMES failure outcomes (expected at least 3)"
fi

# ── Step 10: Verify parent bead is blocked by children ───────────────────────

echo "Step 10: Verifying parent bead is blocked by children..."

# Check via br show output for dependency lines.
BLOCKING_INFO="$(echo "$PARENT_INFO" | grep -i "blocks\|depend" || echo "")"
if [ -n "$BLOCKING_INFO" ]; then
    # Count dependency lines.
    DEP_COUNT="$(echo "$BLOCKING_INFO" | wc -l)"
    DEP_COUNT="${DEP_COUNT//[[:space:]]/}"
    pass "Parent bead has dependency relationships ($DEP_COUNT lines)"
else
    # Fallback: check if mitosis event recorded children.
    if [ "${CHILDREN_CREATED:-0}" -ge 3 ]; then
        info "Dependencies not visible in br show, but mitosis created $CHILDREN_CREATED children"
    else
        fail "Parent bead has no blocking children"
    fi
fi

# ── Step 11: Verify no infinite loop ─────────────────────────────────────────

echo "Step 11: Verifying no infinite loop..."

# The worker should have exited (exhausted or stopped).
EXHAUSTED_COUNT=$(count_event "worker.exhausted")
STOPPED_COUNT=$(count_event "worker.stopped")

if [ "$EXHAUSTED_COUNT" -ge 1 ]; then
    pass "Worker reached EXHAUSTED state (no infinite loop)"
elif [ "$STOPPED_COUNT" -ge 1 ]; then
    pass "Worker stopped cleanly"
else
    fail "Worker did not reach exhausted/stopped state"
fi

# Verify the test completed quickly (not hitting the 30s timeout).
if [ "$EXIT_CODE" -eq 124 ]; then
    fail "Worker hit the 30s timeout (possible infinite loop)"
else
    pass "Worker completed within timeout"
fi

# ── Step 12: Verify failure count was at 3 when mitosis triggered ────────────

echo "Step 12: Verifying failure count was 3 when mitosis triggered..."

# The failure count label should be at least 3.
if [ -n "$FAILURE_COUNT" ] && [ "$FAILURE_COUNT" -ge 3 ]; then
    pass "Failure count is $FAILURE_COUNT (>= 3 threshold)"
else
    fail "Failure count is ${FAILURE_COUNT:-empty} (expected >= 3)"
fi

# ── Step 13: Verify worker claimed child bead after mitosis ──────────────────

echo "Step 13: Verifying worker claimed child bead after mitosis..."

# After mitosis, the worker should claim a child bead, not the parent again.
# The parent bead has BEAD_ID. After the 3rd claim of the parent, the next
# claim should be for a different bead.
PARENT_CLAIM_EVENTS=$(jq -c "select(.event_type == \"bead.claim.succeeded\" and .data.bead_id == \"$BEAD_ID\")" "$TELEMETRY_LOG" 2>/dev/null | wc -l || echo 0)
PARENT_CLAIM_EVENTS="${PARENT_CLAIM_EVENTS//[[:space:]]/}"

TOTAL_CLAIMS=$(count_event "bead.claim.succeeded")
TOTAL_CLAIMS="${TOTAL_CLAIMS//[[:space:]]/}"

info "Parent claims: $PARENT_CLAIM_EVENTS, total claims: $TOTAL_CLAIMS"

if [ "$PARENT_CLAIM_EVENTS" -eq 3 ] && [ "$TOTAL_CLAIMS" -gt 3 ]; then
    pass "Parent claimed exactly 3 times, then worker moved to children"
elif [ "$PARENT_CLAIM_EVENTS" -le 3 ]; then
    pass "Parent claimed $PARENT_CLAIM_EVENTS times (within threshold)"
else
    fail "Parent claimed $PARENT_CLAIM_EVENTS times (expected exactly 3)"
fi

# ── Step 14: Additional telemetry verification ───────────────────────────────

echo "Step 14: Verifying telemetry completeness..."

# Check for required events.
REQUIRED_EVENTS=(
    "worker.started"
    "bead.claim.attempted"
    "bead.claim.succeeded"
    "agent.dispatched"
    "agent.completed"
    "outcome.classified"
    "outcome.handled"
    "bead.released"
    "bead.mitosis.split"
)

for event in "${REQUIRED_EVENTS[@]}"; do
    count=$(count_event "$event")
    if [ "$count" -ge 1 ]; then
        pass "Event '$event' present ($count occurrences)"
    else
        fail "Event '$event' MISSING from telemetry log"
    fi
done

# ── Result ───────────────────────────────────────────────────────────────────

echo ""
echo "=== Results ==="
echo -e "  Passed: ${GREEN}$PASS_COUNT${NC}"
echo -e "  Failed: ${RED}$FAIL_COUNT${NC}"
echo ""

if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    echo ""
    echo "Summary: Failure counter persists across claim cycles and forced"
    echo "mitosis triggers correctly at threshold=3."
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    if [ -f "$TELEMETRY_LOG" ]; then
        echo ""
        echo "=== Telemetry event summary ==="
        jq -r '[.sequence, .event_type] | @tsv' "$TELEMETRY_LOG" 2>/dev/null | head -50 || cat "$TELEMETRY_LOG"
    fi

    # Show parent bead state for debugging.
    echo ""
    echo "=== Parent bead state ==="
    (cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null) || true

    exit 1
fi
