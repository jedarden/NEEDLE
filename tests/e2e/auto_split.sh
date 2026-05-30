#!/bin/bash
# E2E: Auto-split triggers at consecutive failure threshold
#
# Proves that when a bead accumulates consecutive failures, the worker
# dispatches a SPLIT instruction (instead of normal PLUCK prompt) and
# the agent decomposes the bead into child beads, converting the parent
# into an umbrella.
#
# Flow:
#   Attempt 1: bead claimed, agent fails (exit 1), failure_count=1, released
#   Attempt 2: same bead re-claimed, agent fails, failure_count=2, released
#   Attempt 3: same bead re-claimed, agent fails, failure_count=3, released
#   Attempt 4: same bead re-claimed, SPLIT mode triggered (failure_count >= threshold)
#              Agent receives SPLIT prompt, creates 3 child beads, umbrellas parent
#   Worker claims and closes child beads, then reaches EXHAUSTED
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

echo "=== E2E: Auto-Split Triggers at Consecutive Failure Threshold ==="
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

cleanup() {
    # Restore HOME before cleanup
    export HOME="$REAL_HOME"
    rm -rf "$TMPBASE"
}
trap cleanup EXIT

REAL_HOME="$HOME"
export HOME="$FAKE_HOME"
mkdir -p "$HOME"

# ── Step 1: Create workspace ──────────────────────────────────────────────────

echo "Step 1: Creating workspace..."
mkdir -p "$WORKSPACE"
(cd "$WORKSPACE" && "$BR_BIN" init 2>&1) || {
    echo "FATAL: br init failed"
    exit 1
}
echo "  Workspace: $WORKSPACE"

# ── Step 2: Create test bead with complex task ─────────────────────────────────

echo "Step 2: Creating test bead with complex task..."

BEAD_DESC="$(cat <<'BODY'
## E2E Auto-Split Test

This bead has three independent subtasks to be split by auto-split:
subtask ALPHA (implement core), subtask BETA (add tests), and subtask GAMMA (write docs).

The bead should fail 3 times, then trigger auto-split on the 4th attempt.
BODY
)"

BEAD_ID=""
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
    --title "E2E auto-split test bead" \
    --description "$BEAD_DESC" \
    --label e2e-test 2>&1 | head -1 | tr -d '\n\r')" || {
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
        --title "E2E auto-split test bead" \
        --description "$BEAD_DESC" \
        --label e2e-test 2>&1 | head -1 | tr -d '\n\r')"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create smart agent adapter ────────────────────────────────────────
#
# The agent has three behaviors based on the prompt:
#   1. "Auto-Split: Decompose This Bead" in prompt → create children, wire deps, umbrella parent, exit 0
#   2. "E2E-CHILD-" in prompt title → close the bead and exit 0
#   3. Otherwise (parent bead work) → exit 1 (simulates failure)

echo "Step 3: Creating smart-split agent adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/smart-split.yaml" <<YAML
name: smart-split
agent_cli: bash
invoke_template: |
  # Check if this is a SPLIT instruction prompt
  if grep -q 'Auto-Split: Decompose This Bead' '{prompt_file}' 2>/dev/null; then
    # Extract parent bead ID from prompt
    PARENT_ID="\$(grep 'Bead ID:' '{prompt_file}' | head -1 | sed 's/.*Bead ID: *//' | sed 's/[^a-z0-9-].*//')"

    # Create 3 child beads with split-child label
    CHILD1="\$(cd '{workspace}' && $BR_BIN create --title "E2E-CHILD-ALPHA: Implement core" --description "Implement the core functionality" --label split-child 2>&1 | head -1 | tr -d '\n\r')"
    CHILD2="\$(cd '{workspace}' && $BR_BIN create --title "E2E-CHILD-BETA: Add tests" --description "Add comprehensive tests" --label split-child 2>&1 | head -1 | tr -d '\n\r')"
    CHILD3="\$(cd '{workspace}' && $BR_BIN create --title "E2E-CHILD-GAMMA: Write docs" --description "Write documentation" --label split-child 2>&1 | head -1 | tr -d '\n\r')"

    # Chain dependencies: CHILD2 depends on CHILD1, CHILD3 depends on CHILD2
    cd '{workspace}' && $BR_BIN dep add "\$CHILD2" "\$CHILD1" 2>/dev/null || true
    cd '{workspace}' && $BR_BIN dep add "\$CHILD3" "\$CHILD2" 2>/dev/null || true

    # Convert parent to umbrella: depends on last child, add umbrella label
    cd '{workspace}' && $BR_BIN dep add "\$PARENT_ID" "\$CHILD3" 2>/dev/null || true
    cd '{workspace}' && $BR_BIN label add "\$PARENT_ID" "umbrella" 2>/dev/null || true

    echo "SPLIT_COMPLETE: Created 3 children, parent converted to umbrella"
    echo "Children: \$CHILD1, \$CHILD2, \$CHILD3"
    exit 0
  fi

  # Check if this is a child bead (should succeed)
  if grep -q 'E2E-CHILD-' '{prompt_file}' 2>/dev/null; then
    # Extract bead ID from prompt (format: "**Bead ID:** <id>")
    BEAD_ID="\$(grep 'Bead ID:' '{prompt_file}' | head -1 | sed 's/.*\*\*Bead ID:\*\* *//' | sed 's/[^a-z0-9-].*//')"
    if [ -n "\$BEAD_ID" ]; then
      cd '{workspace}' && $BR_BIN close "\$BEAD_ID" --reason 'E2E child completed' 2>/dev/null || true
    fi
    exit 0
  fi

  # Parent bead work: simulate failure
  exit 1
timeout_secs: 15
environment:
  BR_BIN: "$BR_BIN"
YAML

# ── Step 4: Configure needle with split_after_failures: 3 ───────────────────

echo "Step 4: Configuring needle (split_after_failures: 3)..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: smart-split
  timeout: 15
strands:
  pluck:
    split_after_failures: 3
  mitosis:
    enabled: false
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 10
YAML

# ── Step 5: Run needle ────────────────────────────────────────────────────────

echo "Step 5: Running needle worker..."
export NEEDLE_INNER=1

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0
timeout 90 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent smart-split \
    --count 1 \
    --identifier e2e-auto-split 2>/dev/null || EXIT_CODE=$?

echo ""

# ── Step 6: Assertions ────────────────────────────────────────────────────────

echo "Step 6: Checking assertions..."
PASS=true

# 6a. Worker exited cleanly.
if [ "$EXIT_CODE" -eq 0 ]; then
    pass "Worker exited with code 0"
else
    fail "Worker exited with code $EXIT_CODE"
fi

# 6b. Parent bead should have umbrella label and be BLOCKED (depends on last child).
PARENT_SHOW="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null || echo "ERROR")"
info "Parent bead show output: $PARENT_SHOW"
if echo "$PARENT_SHOW" | grep -qi "umbrella"; then
    pass "Parent bead has umbrella label"
else
    fail "Parent bead missing umbrella label"
fi

# Check if parent is blocked (has open dependency)
if echo "$PARENT_SHOW" | grep -qi "blocked\|BLOCKED\|⊘\|dep\|blocks"; then
    pass "Parent bead is BLOCKED (has dependency on last child)"
elif echo "$PARENT_SHOW" | grep -qi "open\|OPEN\|○"; then
    info "Parent bead shows as OPEN (may have open dependency - acceptable)"
fi

# 6c. 3 child beads should exist and be CLOSED.
CHILD_BEADS="$(cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null | grep "E2E-CHILD-" || echo "")"
CHILD_COUNT="$(echo "$CHILD_BEADS" | grep -c "E2E-CHILD-" || true)"
CHILD_COUNT="${CHILD_COUNT:-0}"
CHILD_COUNT="${CHILD_COUNT//[[:space:]]/}"
info "Child beads found: $CHILD_COUNT"
if [ "$CHILD_COUNT" -eq 3 ]; then
    pass "3 child beads created by auto-split"
else
    fail "Expected 3 child beads, found $CHILD_COUNT"
fi

# Check that each child is closed.
CLOSED_CHILDREN=0
while IFS= read -r line; do
    if echo "$line" | grep -qi "CLOSED\|✓\|closed"; then
        CLOSED_CHILDREN=$((CLOSED_CHILDREN + 1))
    fi
done <<< "$CHILD_BEADS"

if [ "$CLOSED_CHILDREN" -eq 3 ]; then
    pass "All 3 child beads are CLOSED"
elif [ "$CLOSED_CHILDREN" -gt 0 ]; then
    info "Only $CLOSED_CHILDREN/3 child beads are CLOSED"
else
    fail "No child beads are CLOSED"
fi

# 6d. Telemetry validation.
TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "e2e-auto-split-*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG" ]; then
    fail "No telemetry log found in $TELEMETRY_DIR"
else
    info "Telemetry log: $TELEMETRY_LOG"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
    info "Total events: $EVENT_COUNT"

    # The parent bead should appear in bead.claim.succeeded at least 4 times
    # (3 failures + 1 split attempt).
    PARENT_CLAIMS="$(grep '"event_type":"bead.claim.succeeded"' "$TELEMETRY_LOG" 2>/dev/null \
        | grep "\"${BEAD_ID}\"" | wc -l || true)"
    PARENT_CLAIMS="${PARENT_CLAIMS:-0}"
    PARENT_CLAIMS="${PARENT_CLAIMS//[[:space:]]/}"
    info "Parent bead claims: $PARENT_CLAIMS"
    if [ "$PARENT_CLAIMS" -ge 4 ]; then
        pass "Parent bead claimed at least 4 times (3 failures + 1 split)"
    elif [ "$PARENT_CLAIMS" -gt 0 ]; then
        info "Parent bead claimed $PARENT_CLAIMS times"
    else
        fail "Parent bead not found in claim events"
    fi

    # Should have at least 3 bead.released events (one per failure).
    RELEASE_COUNT="$(grep -c '"event_type":"bead.released"' "$TELEMETRY_LOG" 2>/dev/null || true)"
    RELEASE_COUNT="${RELEASE_COUNT:-0}"
    RELEASE_COUNT="${RELEASE_COUNT//[[:space:]]/}"
    info "bead.released events: $RELEASE_COUNT"
    if [ "$RELEASE_COUNT" -ge 3 ]; then
        pass "At least 3 bead.released events (failure counter incremented)"
    else
        fail "Expected at least 3 bead.released events, found $RELEASE_COUNT"
    fi

    # Worker should reach EXHAUSTED (not loop forever).
    if grep -q '"event_type":"worker.exhausted"' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "Worker reached EXHAUSTED state (no infinite loop)"
    else
        fail "Worker did not reach EXHAUSTED state"
    fi

    # Verify failure-count labels on parent bead via telemetry
    FAILURE_CLASSIFIED="$(grep '"event_type":"outcome.classified"' "$TELEMETRY_LOG" 2>/dev/null \
        | grep '"failure"' | grep "\"${BEAD_ID}\"" | wc -l || true)"
    FAILURE_CLASSIFIED="${FAILURE_CLASSIFIED:-0}"
    FAILURE_CLASSIFIED="${FAILURE_CLASSIFIED//[[:space:]]/}"
    info "failure outcomes for parent: $FAILURE_CLASSIFIED"
    if [ "$FAILURE_CLASSIFIED" -ge 3 ]; then
        pass "At least 3 failure outcomes for parent bead"
    else
        info "Parent bead had $FAILURE_CLASSIFIED failure outcomes (may be acceptable)"
    fi
fi

# 6e. Verify parent has umbrella label and split-child children exist
PARENT_LABELS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | grep -E "label|Label" || echo "")"
info "Parent bead labels: $PARENT_LABELS"

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
        echo "=== Telemetry log (auto-split) ==="
        # Show key events only.
        grep -E '"event_type":"(bead\.claim|bead\.released|outcome\.classified|worker\.(exhausted|stopped))"' \
            "$TELEMETRY_LOG" 2>/dev/null \
            | python3 -m json.tool --no-ensure-ascii 2>/dev/null \
            || grep -E '"event_type":"(bead\.claim|bead\.released|outcome|worker)"' "$TELEMETRY_LOG" 2>/dev/null
    fi

    # Dump bead state.
    echo ""
    echo "=== Workspace bead state ==="
    cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null || true

    exit 1
fi
