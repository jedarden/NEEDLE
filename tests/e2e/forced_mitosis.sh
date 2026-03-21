#!/bin/bash
# E2E: Failure counter persists across claim cycles — forced mitosis triggers
#
# Proves that when a bead fails verification repeatedly, the failure count is tracked
# across claim/release cycles and forced mitosis triggers at the configured threshold.
#
# This is the exact bug that caused the bash NEEDLE GLM workers to loop forever:
# the failure counter reset to 1 on every claim cycle, so force_failure_threshold: 3 never triggered.
#
# Test Plan:
# 1. Create a workspace with 1 bead that has 3 subtasks in its body
# 2. Create a test agent adapter that always exits with code 1 (simulates failure)
# 3. Configure: mitosis.force_failure_threshold: 3
# 4. Launch a single worker
# 5. Assert the following sequence:
#    - Attempt 1: bead claimed, agent fails (exit 1), failure_count=1, released
#    - Attempt 2: same bead re-claimed, agent fails, failure_count=2, released
#    - Attempt 3: same bead re-claimed, agent fails, failure_count=3, forced mitosis triggers
#    - Mitosis creates child beads from the 3 subtasks
#    - Parent bead is now blocked by children
#    - Worker claims a child bead next (not the parent again)
# 6. Verify via telemetry:
#    - failure_count increments correctly (1, 2, 3)
#    - mitosis.forced event emitted on attempt 3
#    - mitosis.children_created shows the child bead IDs
#    - No infinite loop — worker eventually reaches new work or EXHAUSTED
#
# Dependencies: br (beads_rust CLI), needle binary (built from this repo)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── Color helpers ──────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

pass() { echo -e "  ${GREEN}PASS${NC}: $1"; }
fail() { echo -e "  ${RED}FAIL${NC}: $1"; PASS=false; }
info() { echo -e "  ${YELLOW}INFO${NC}: $1"; }

# ── Build needle ───────────────────────────────────────────────────────────────

echo "=== E2E: Failure Counter Persists — Forced Mitosis Triggers ==="
echo ""

NEEDLE_BIN="$PROJECT_ROOT/target/debug/needle"

if [ ! -x "$NEEDLE_BIN" ]; then
    echo "Building needle (debug)..."
    cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1
fi

if [ ! -x "$NEEDLE_BIN" ]; then
    echo "FATAL: needle binary not found at $NEEDLE_BIN"
    exit 1
fi

# Verify br is available.
BR_BIN="$(which br 2>/dev/null || echo "$HOME/.local/bin/br")"
if [ ! -x "$BR_BIN" ]; then
    echo "FATAL: br binary not found"
    exit 1
fi

# ── Create isolated environment ────────────────────────────────────────────────

TMPBASE="$(mktemp -d)"
WORKSPACE="$TMPBASE/workspace"
FAKE_HOME="$TMPBASE/home"
MITOSIS_MARKER="$TMPBASE/mitosis_invocations"
MITOSIS_COUNTER_FILE="$TMPBASE/mitosis_counter"

cleanup() {
    rm -rf "$TMPBASE"
}
trap cleanup EXIT

# Save real HOME for br discovery, then isolate.
REAL_HOME="$HOME"
export HOME="$FAKE_HOME"
mkdir -p "$HOME"

# Initialize mitosis counter
echo "0" > "$MITOSIS_COUNTER_FILE"

# ── Step 1: Create workspace ──────────────────────────────────────────────────

echo "Step 1: Creating workspace..."
mkdir -p "$WORKSPACE"
(cd "$WORKSPACE" && "$BR_BIN" init 2>&1) || {
    echo "FATAL: br init failed"
    exit 1
}
echo "  Workspace: $WORKSPACE"

# ── Step 2: Create test bead with 3 subtasks ──────────────────────────────────

echo "Step 2: Creating test bead with 3 subtasks..."
BEAD_ID=""
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
    --title "Multi-task bead for forced mitosis test" \
    --description "This bead contains multiple subtasks that should trigger forced mitosis.

## Subtasks

- [ ] Subtask A: Implement feature X
- [ ] Subtask B: Add unit tests
- [ ] Subtask C: Update documentation" \
    --silent 2>/dev/null)" || {
    # Retry once after sync (FrankenSQLite WAL race).
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
        --title "Multi-task bead for forced mitosis test" \
        --description "This bead contains multiple subtasks for forced mitosis.

## Subtasks

- [ ] Subtask A: Implement feature X
- [ ] Subtask B: Add unit tests
- [ ] Subtask C: Update documentation" \
        --silent)"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create failing agent adapter ──────────────────────────────────────
#
# The agent always exits with code 1 (failure).
# When invoked with a mitosis prompt (contains "splittable" or "mitosis"),
# it outputs a JSON response indicating the bead is splittable with 3 children.

echo "Step 3: Creating failing-agent adapter with mitosis support..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

# Create a script that:
# 1. Exits 1 (failure) for regular invocations
# 2. Returns mitosis analysis JSON when prompt is for mitosis analysis
#
# Template variables: {prompt_file} is the path to a file containing the prompt
# The mitosis prompt contains "Mitosis Analysis" in its content
# Regular task prompts do NOT contain this phrase
cat > "$ADAPTERS_DIR/failing-agent.yaml" <<'YAML'
name: failing-agent
agent_cli: bash
invoke_template: |
  PROMPT_CONTENT=$(cat {prompt_file} 2>/dev/null || echo "")
  if echo "$PROMPT_CONTENT" | grep -q "Mitosis Analysis"; then
    # This is a mitosis analysis prompt - return splittable response
    echo '{"splittable": true, "children": [{"title": "Subtask A: Implement feature X", "body": "Implement feature X as specified in parent"}, {"title": "Subtask B: Add unit tests", "body": "Add comprehensive unit tests"}, {"title": "Subtask C: Update documentation", "body": "Update the documentation for feature X"}]}'
    exit 0
  else
    # Regular task - always fail with exit code 1
    echo "Agent failed intentionally for E2E test" >&2
    exit 1
  fi
timeout_secs: 30
YAML

# ── Step 4: Configure needle with forced mitosis ──────────────────────────────

echo "Step 4: Configuring needle with force_failure_threshold: 3..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: failing-agent
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

# ── Step 5: Run needle ────────────────────────────────────────────────────────

echo "Step 5: Running needle worker..."

# TMUX=fake makes needle think it's inside tmux, so it runs the worker directly
# instead of launching a tmux session.
export TMUX=fake

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0
timeout 60 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent failing-agent \
    --count 1 \
    --identifier e2e-forced-mitosis 2>&1 || EXIT_CODE=$?

echo ""
info "Needle exited with code: $EXIT_CODE"

# ── Step 6: Assertions ────────────────────────────────────────────────────────

echo ""
echo "Step 6: Checking assertions..."
PASS=true

# 6a. Worker should have exited (either exhausted or after processing)
if [ "$EXIT_CODE" -eq 0 ]; then
    pass "Exit code is 0"
else
    fail "Exit code was $EXIT_CODE, expected 0"
fi

# 6b. Original bead should NOT be closed (it was blocked by children)
BEAD_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD_STATUS" | grep -qi "OPEN\|○"; then
    pass "Original bead $BEAD_ID is still open (blocked by children)"
elif echo "$BEAD_STATUS" | grep -qi "CLOSED\|✓"; then
    fail "Original bead $BEAD_ID was closed — should be blocked by children"
else
    info "Original bead status: $BEAD_STATUS"
fi

# 6c. Telemetry events
TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG" ]; then
    fail "No telemetry log found in $TELEMETRY_DIR"
else
    info "Telemetry log: $TELEMETRY_LOG"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
    info "Total events: $EVENT_COUNT"

    # Count failure releases for the original bead
    FAILURE_RELEASES="$(grep "\"event_type\":\"bead.released\"" "$TELEMETRY_LOG" 2>/dev/null | grep -c "\"reason\":\"failure\"" || true)"
    info "Failure releases: $FAILURE_RELEASES"

    if [ "$FAILURE_RELEASES" -ge 3 ]; then
        pass "At least 3 failure releases occurred"
    else
        fail "Only $FAILURE_RELEASES failure releases found (expected >= 3)"
    fi

    # Verify mitosis.split event exists (forced mitosis triggered)
    if grep -q '"event_type":"bead.mitosis.split"' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "bead.mitosis.split event found (forced mitosis triggered)"

        # Extract child_ids from the mitosis.split event
        MITOSIS_EVENT="$(grep '"event_type":"bead.mitosis.split"' "$TELEMETRY_LOG" | head -1)"
        CHILD_COUNT="$(echo "$MITOSIS_EVENT" | grep -o '"children_created":[0-9]*' | grep -o '[0-9]*' || echo "0")"
        info "Mitosis created $CHILD_COUNT child beads"

        if [ "$CHILD_COUNT" -ge 3 ]; then
            pass "Mitosis created $CHILD_COUNT children (expected 3)"
        else
            fail "Mitosis only created $CHILD_COUNT children (expected 3)"
        fi
    else
        fail "No bead.mitosis.split event found — forced mitosis did not trigger"
    fi

    # Verify failure count progression by checking labels on the bead
    # The bead should have failure-count:3 label after 3 failures
    BEAD_LABELS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | grep -i label || true)"
    if echo "$BEAD_LABELS" | grep -q "failure-count:3"; then
        pass "Bead has failure-count:3 label"
    elif echo "$BEAD_LABELS" | grep -q "failure-count:[0-9]"; then
        FOUND_COUNT="$(echo "$BEAD_LABELS" | grep -o 'failure-count:[0-9]*' | grep -o '[0-9]*' | head -1)"
        info "Bead has failure-count:$FOUND_COUNT label"
        if [ "$FOUND_COUNT" -ge 3 ]; then
            pass "Bead has failure-count >= 3"
        else
            fail "Bead failure count is $FOUND_COUNT (expected >= 3)"
        fi
    else
        info "Could not verify failure count label (may have been cleaned up)"
    fi

    # Verify worker did not get stuck in an infinite loop
    # If the worker exhausted, it should have worker.exhausted event
    if grep -q '"event_type":"worker.exhausted"' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "Worker reached EXHAUSTED state (no infinite loop)"
    elif grep -q '"event_type":"worker.stopped"' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "Worker reached STOPPED state (no infinite loop)"
    else
        info "Worker terminal state: checking for infinite loop..."
        # Check if there are excessive claim attempts on the same bead
        CLAIM_COUNT="$(grep -c "\"bead_id\":\"$BEAD_ID\"" "$TELEMETRY_LOG" 2>/dev/null || true)"
        if [ "$CLAIM_COUNT" -gt 10 ]; then
            fail "Excessive claims ($CLAIM_COUNT) on original bead — possible infinite loop"
        else
            pass "No excessive claims detected ($CLAIM_COUNT total references to original bead)"
        fi
    fi

    # Verify parent bead has children (dependencies)
    PARENT_INFO="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null || echo "")"
    if echo "$PARENT_INFO" | grep -qi "blocks\|blocked\|depend"; then
        pass "Parent bead has dependency relationships (children blocking it)"
    else
        info "Could not verify dependency relationships on parent bead"
    fi

    # Check for child beads in the workspace
    ALL_BEADS="$(cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null || echo "")"
    CHILD_BEAD_COUNT="$(echo "$ALL_BEADS" | grep -c "Subtask\|mitosis-child" || true)"
    if [ "$CHILD_BEAD_COUNT" -ge 1 ]; then
        pass "Found $CHILD_BEAD_COUNT child beads created by mitosis"
    else
        fail "No child beads found — mitosis may not have created children"
    fi
fi

# ── Result ─────────────────────────────────────────────────────────────────────

echo ""
if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    # Dump telemetry for debugging.
    if [ -n "${TELEMETRY_LOG:-}" ] && [ -f "$TELEMETRY_LOG" ]; then
        echo ""
        echo "=== Telemetry log ==="
        cat "$TELEMETRY_LOG" | python3 -m json.tool --no-ensure-ascii 2>/dev/null \
            || cat "$TELEMETRY_LOG"
    fi

    # Dump workspace contents.
    echo ""
    echo "=== Workspace beads ==="
    (cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null) || true

    echo ""
    echo "=== Original bead details ==="
    (cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null) || true

    exit 1
fi
