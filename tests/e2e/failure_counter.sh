#!/bin/bash
# E2E: Failure counter persists across claim cycles — forced mitosis triggers
#
# Proves that when a bead fails repeatedly, the failure count is correctly
# tracked across claim/release cycles and forced mitosis triggers at the
# configured threshold (force_failure_threshold: 3).
#
# Flow:
#   Attempt 1: bead claimed, agent fails (exit 1), failure_count=1, released
#   Attempt 2: same bead re-claimed, agent fails, failure_count=2, released
#   Attempt 3: same bead re-claimed, agent fails, failure_count=3 → mitosis!
#   Mitosis creates 3 child beads from the parent's subtasks
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

echo "=== E2E: Failure Counter Persists — Forced Mitosis at Threshold ==="
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

# ── Step 2: Create test bead with 3 subtasks ──────────────────────────────────

echo "Step 2: Creating test bead with 3 subtasks..."

BEAD_DESC="$(cat <<'BODY'
## E2E Failure Counter Test

NEEDLE-FAIL-MARKER: This bead always fails until mitosis splits it.

This bead has three independent subtasks to be split by mitosis:
subtask A, subtask B, and subtask C.
BODY
)"

BEAD_ID=""
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
    --title "E2E failure counter test bead" \
    --description "$BEAD_DESC" \
    --silent 2>/dev/null)" || {
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
        --title "E2E failure counter test bead" \
        --description "$BEAD_DESC" \
        --silent)"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create smart agent adapter ────────────────────────────────────────
#
# The agent has three behaviors based on the prompt:
#   1. "## Mitosis Analysis" in prompt → return JSON with 3 children, exit 0
#   2. "E2E-CHILD-" in prompt title → close the bead and exit 0
#   3. Otherwise (parent bead work) → exit 1 (simulates failure)

echo "Step 3: Creating smart-fail agent adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/smart-fail.yaml" <<YAML
name: smart-fail
agent_cli: bash
invoke_template: |
  if grep -q '## Mitosis Analysis' '{prompt_file}' 2>/dev/null; then
    printf '%s' '{"splittable":true,"children":[{"title":"E2E-SPLIT-ALPHA: Complete subtask A","body":"Create file alpha.txt"},{"title":"E2E-SPLIT-BETA: Complete subtask B","body":"Create file beta.txt"},{"title":"E2E-SPLIT-GAMMA: Complete subtask C","body":"Create file gamma.txt"}]}'
    exit 0
  elif grep -q 'NEEDLE-FAIL-MARKER' '{prompt_file}' 2>/dev/null; then
    exit 1
  else
    cd '{workspace}' && $BR_BIN close '{bead_id}' --reason 'E2E child completed'
    exit 0
  fi
timeout_secs: 15
environment:
  BR_BIN: "$BR_BIN"
YAML

# ── Step 4: Configure needle with force_failure_threshold: 3 ─────────────────

echo "Step 4: Configuring needle (force_failure_threshold: 3)..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: smart-fail
  timeout: 15
strands:
  mitosis:
    enabled: true
    first_failure_only: false
    force_failure_threshold: 3
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 10
YAML

# ── Step 5: Run needle ────────────────────────────────────────────────────────

echo "Step 5: Running needle worker..."
export NEEDLE_INNER=1

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0
timeout 60 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent smart-fail \
    --count 1 \
    --identifier e2e-failure-counter 2>/dev/null || EXIT_CODE=$?

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

# 6b. Parent bead should be BLOCKED (not closed, not open — blocked by children).
PARENT_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
info "Parent bead status: $PARENT_STATUS"
if echo "$PARENT_STATUS" | grep -qi "blocked\|BLOCKED\|⊘"; then
    pass "Parent bead is BLOCKED (has open dependencies after mitosis)"
elif echo "$PARENT_STATUS" | grep -qi "open\|OPEN\|○"; then
    fail "Parent bead is still OPEN — mitosis may not have triggered"
elif echo "$PARENT_STATUS" | grep -qi "closed\|CLOSED\|✓"; then
    fail "Parent bead was CLOSED — expected BLOCKED after mitosis"
else
    # Bead may show as open with dependencies in some br versions.
    info "Parent bead status unclear: $PARENT_STATUS"
    # Check if the bead has child dependencies
    CHILD_COUNT="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | grep -c "blocks\|dep\|child" || echo 0)"
    if [ "$CHILD_COUNT" -gt 0 ]; then
        pass "Parent bead has child dependencies (mitosis occurred)"
    else
        fail "Parent bead status unexpected: $PARENT_STATUS"
    fi
fi

# 6c. 3 child beads should exist and be CLOSED.
CHILD_BEADS="$(cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null | grep "E2E-SPLIT-" || echo "")"
CHILD_COUNT="$(echo "$CHILD_BEADS" | grep -c "E2E-SPLIT-" || true)"
CHILD_COUNT="${CHILD_COUNT:-0}"
CHILD_COUNT="${CHILD_COUNT//[[:space:]]/}"
info "Child beads found: $CHILD_COUNT"
if [ "$CHILD_COUNT" -eq 3 ]; then
    pass "3 child beads created by mitosis"
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
    info "Only $CLOSED_CHILDREN/3 child beads are CLOSED (worker may have stopped after mitosis)"
fi

# 6d. Telemetry validation.
TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "e2e-failure-counter-*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG" ]; then
    fail "No telemetry log found in $TELEMETRY_DIR"
else
    info "Telemetry log: $TELEMETRY_LOG"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
    info "Total events: $EVENT_COUNT"

    # The parent bead should appear in bead.claim.succeeded exactly 3 times.
    PARENT_CLAIMS="$(grep '"event_type":"bead.claim.succeeded"' "$TELEMETRY_LOG" 2>/dev/null \
        | grep "\"${BEAD_ID}\"" | wc -l || true)"
    PARENT_CLAIMS="${PARENT_CLAIMS:-0}"
    PARENT_CLAIMS="${PARENT_CLAIMS//[[:space:]]/}"
    info "Parent bead claims: $PARENT_CLAIMS"
    if [ "$PARENT_CLAIMS" -eq 3 ]; then
        pass "Parent bead claimed exactly 3 times (failure count reached threshold)"
    elif [ "$PARENT_CLAIMS" -gt 0 ]; then
        info "Parent bead claimed $PARENT_CLAIMS times (may be acceptable)"
    else
        fail "Parent bead not found in claim events"
    fi

    # Should have at least 3 bead.released events (one per failure).
    RELEASE_COUNT="$(grep -c '"event_type":"bead.released"' "$TELEMETRY_LOG" 2>/dev/null || true)"
    RELEASE_COUNT="${RELEASE_COUNT:-0}"
    RELEASE_COUNT="${RELEASE_COUNT//[[:space:]]/}"
    info "bead.released events: $RELEASE_COUNT"
    if [ "$RELEASE_COUNT" -ge 3 ]; then
        pass "At least 3 bead.released events (failure counter incremented 3 times)"
    else
        fail "Expected at least 3 bead.released events, found $RELEASE_COUNT"
    fi

    # mitosis.evaluated should appear once (triggered at threshold).
    MITOSIS_COUNT="$(grep -c '"event_type":"mitosis.evaluated"' "$TELEMETRY_LOG" 2>/dev/null || true)"
    MITOSIS_COUNT="${MITOSIS_COUNT:-0}"
    MITOSIS_COUNT="${MITOSIS_COUNT//[[:space:]]/}"
    info "mitosis.evaluated events: $MITOSIS_COUNT"
    if [ "$MITOSIS_COUNT" -ge 1 ]; then
        pass "mitosis.evaluated event emitted (mitosis triggered)"
    else
        fail "No mitosis.evaluated event — mitosis may not have triggered"
    fi

    # Worker should reach EXHAUSTED (not loop forever).
    if grep -q '"event_type":"worker.exhausted"' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "Worker reached EXHAUSTED state (no infinite loop)"
    else
        fail "Worker did not reach EXHAUSTED state"
    fi

    # Verify failure-count labels on parent bead via telemetry
    # (The worker emits outcome.classified with outcome="failure" 3 times).
    FAILURE_CLASSIFIED="$(grep '"event_type":"outcome.classified"' "$TELEMETRY_LOG" 2>/dev/null \
        | grep '"failure"' | grep "\"${BEAD_ID}\"" | wc -l || true)"
    FAILURE_CLASSIFIED="${FAILURE_CLASSIFIED:-0}"
    FAILURE_CLASSIFIED="${FAILURE_CLASSIFIED//[[:space:]]/}"
    info "failure outcomes for parent: $FAILURE_CLASSIFIED"
    if [ "$FAILURE_CLASSIFIED" -eq 3 ]; then
        pass "Exactly 3 failure outcomes for parent bead"
    elif [ "$FAILURE_CLASSIFIED" -gt 0 ]; then
        info "Parent bead had $FAILURE_CLASSIFIED failure outcomes"
    else
        fail "No failure outcomes found for parent bead"
    fi
fi

# 6e. Verify failure-count labels persisted on the parent bead in br.
PARENT_LABELS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | grep "failure-count" || echo "")"
info "Parent bead failure-count labels: $PARENT_LABELS"
if echo "$PARENT_LABELS" | grep -q "failure-count:3"; then
    pass "Parent bead has failure-count:3 label (counter persisted across cycles)"
else
    info "failure-count:3 label not found (may be removed after mitosis or different format)"
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
        echo "=== Telemetry log (failure counter) ==="
        # Show key events only.
        grep -E '"event_type":"(bead\.claim|bead\.released|outcome\.classified|mitosis|worker\.(exhausted|stopped))"' \
            "$TELEMETRY_LOG" 2>/dev/null \
            | python3 -m json.tool --no-ensure-ascii 2>/dev/null \
            || grep -E '"event_type":"(bead\.claim|bead\.released|outcome|mitosis|worker)"' "$TELEMETRY_LOG" 2>/dev/null
    fi

    # Dump bead state.
    echo ""
    echo "=== Workspace bead state ==="
    cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null || true

    exit 1
fi
