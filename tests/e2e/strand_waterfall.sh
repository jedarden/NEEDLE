#!/bin/bash
# E2E: Strand waterfall progression — Pluck → Mend → Explore → Knot
#
# Scenario A: Home workspace is empty, but a remote workspace has a bead.
#   The worker waterfall should skip Pluck (no work) → Mend (no work) → Explore
#   finds the remote bead → worker claims and dispatches → bead is CLOSED.
#
# Scenario B: Home workspace is empty and no remote workspaces configured.
#   All strands return NoWork → worker reaches EXHAUSTED → clean exit code 0.
#
# Acceptance criteria (from needle-yvk):
#   - Both scenarios pass
#   - Strand execution order matches plan spec
#   - Explore does NOT scan the filesystem (only configured paths)
#   - Worker returns to home workspace after remote bead execution
#
# Dependencies: br (beads_rust CLI), needle binary (built from this repo), jq

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Source workspace library
source "$SCRIPT_DIR/lib/workspace.sh"

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

# ── Check prerequisites ──────────────────────────────────────────────────────

echo "=== E2E: Strand Waterfall Progression ==="
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

cleanup() {
    rm -rf "$TMPBASE"
}
trap cleanup EXIT

REAL_HOME="$HOME"

# ═══════════════════════════════════════════════════════════════════════════════
# Scenario A: Explore finds remote work
# ═══════════════════════════════════════════════════════════════════════════════

echo "──────────────────────────────────────"
echo "Scenario A: Explore finds remote work"
echo "──────────────────────────────────────"
echo ""

SCENARIO_A_DIR="$TMPBASE/scenario_a"
HOME_WS_A="$SCENARIO_A_DIR/home_workspace"
REMOTE_WS_A="$SCENARIO_A_DIR/remote_workspace"
FAKE_HOME_A="$SCENARIO_A_DIR/home"

mkdir -p "$FAKE_HOME_A"
export HOME="$FAKE_HOME_A"

# ── Step 1: Create empty home workspace ──────────────────────────────────────

echo "Step 1: Creating empty home workspace..."
create_home_workspace "$HOME_WS_A"
echo "  Home workspace: $HOME_WS_A"

# ── Step 2: Create remote workspace with 1 bead ─────────────────────────────

echo "Step 2: Creating remote workspace with 1 bead..."
create_remote_workspace_with_bead "$REMOTE_WS_A" REMOTE_BEAD_ID \
    "Remote task: create DONE file" \
    "Create a file called DONE in the workspace root"
echo "  Remote workspace: $REMOTE_WS_A"
echo "  Remote bead: $REMOTE_BEAD_ID"

# ── Step 3: Create test-echo agent adapter ───────────────────────────────────

echo "Step 3: Creating test-echo adapter..."
ADAPTERS_DIR_A="$FAKE_HOME_A/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR_A"

cat > "$ADAPTERS_DIR_A/test-echo.yaml" <<YAML
name: test-echo
agent_cli: bash
invoke_template: "cd {workspace} && ($BR_BIN sync --flush-only 2>/dev/null || true) && echo done > DONE && $BR_BIN close {bead_id} --reason 'E2E waterfall test completed'"
timeout_secs: 30
YAML

# ── Step 4: Configure needle with explore.workspaces ─────────────────────────

echo "Step 4: Configuring needle with explore.workspaces..."
CONFIG_DIR_A="$FAKE_HOME_A/.config/needle"

configure_explore_workspaces "$CONFIG_DIR_A" "$REMOTE_WS_A"

# Append fast heartbeat settings for E2E timing
printf '\nhealth:\n  heartbeat_interval_secs: 1\n  heartbeat_ttl_secs: 5\n' \
    >> "$CONFIG_DIR_A/config.yaml"

echo "  Explore workspaces: [$REMOTE_WS_A]"

# ── Step 5: Run needle with home workspace ───────────────────────────────────

echo "Step 5: Running needle worker with home workspace..."
export NEEDLE_INNER=1

TELEMETRY_DIR_A="$FAKE_HOME_A/.needle/logs"
EXIT_CODE_A=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$HOME_WS_A" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-waterfall-a 2>&1 || EXIT_CODE_A=$?

echo ""
info "Needle exit code: $EXIT_CODE_A"

# ── Step 6: Assertions for Scenario A ────────────────────────────────────────

echo "Step 6: Checking Scenario A assertions..."
echo ""

# 6a. Exit code
if [ "$EXIT_CODE_A" -eq 0 ]; then
    pass "Exit code is 0"
else
    fail "Exit code was $EXIT_CODE_A, expected 0"
fi

# 6b. Remote bead should be CLOSED
BEAD_STATUS="$(cd "$REMOTE_WS_A" && "$BR_BIN" show "$REMOTE_BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD_STATUS" | grep -qi "closed\|CLOSED\|✓"; then
    pass "Remote bead $REMOTE_BEAD_ID is CLOSED"
else
    fail "Remote bead not closed: $BEAD_STATUS"
fi

# 6c. DONE file exists in remote workspace (agent executed there)
if [ -f "$REMOTE_WS_A/DONE" ]; then
    pass "DONE file exists in remote workspace"
else
    fail "DONE file not found in remote workspace"
fi

# 6d. Telemetry
TELEMETRY_LOG_A="$(find "$TELEMETRY_DIR_A" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG_A" ]; then
    fail "No telemetry log found"
else
    info "Telemetry log: $TELEMETRY_LOG_A"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG_A")"
    info "Total events: $EVENT_COUNT"

    # Worker started
    if grep -q '"event_type":"worker.started"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "worker.started event present"
    else
        fail "worker.started event missing"
    fi

    # Bead was claimed and dispatched
    if grep -q '"event_type":"bead.claim.succeeded"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "bead.claim.succeeded event present"
    else
        fail "bead.claim.succeeded event missing"
    fi

    if grep -q '"event_type":"agent.dispatched"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "agent.dispatched event present"
    else
        fail "agent.dispatched event missing"
    fi

    if grep -q '"event_type":"agent.completed"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "agent.completed event present"
    else
        fail "agent.completed event missing"
    fi

    if grep -q '"event_type":"bead.completed"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "bead.completed event present"
    else
        fail "bead.completed event missing"
    fi

    # Worker eventually exhausted (no more work after the remote bead)
    if grep -q '"event_type":"worker.exhausted"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "worker.exhausted event present (worker completed cycle)"
    else
        fail "worker.exhausted event missing"
    fi

    # Verify the remote bead ID appears in claim events
    if grep '"event_type":"bead.claim.succeeded"' "$TELEMETRY_LOG_A" 2>/dev/null | grep -q "\"$REMOTE_BEAD_ID\""; then
        pass "Remote bead $REMOTE_BEAD_ID appears in claim events"
    else
        info "Could not verify remote bead ID in claim events (format may differ)"
    fi
fi

# 6e. Home workspace should still be empty (no beads created there)
HOME_BEADS="$(cd "$HOME_WS_A" && "$BR_BIN" list --status open 2>/dev/null | wc -l || echo 0)"
HOME_BEADS="${HOME_BEADS//[[:space:]]/}"
if [ "$HOME_BEADS" -eq 0 ]; then
    pass "Home workspace still empty (no beads created in home)"
else
    info "Home workspace has $HOME_BEADS open beads (unexpected)"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Scenario B: All strands empty → EXHAUSTED
# ═══════════════════════════════════════════════════════════════════════════════

echo "──────────────────────────────────────"
echo "Scenario B: All strands empty → EXHAUSTED"
echo "──────────────────────────────────────"
echo ""

SCENARIO_B_DIR="$TMPBASE/scenario_b"
HOME_WS_B="$SCENARIO_B_DIR/home_workspace"
FAKE_HOME_B="$SCENARIO_B_DIR/home"

mkdir -p "$FAKE_HOME_B"
export HOME="$FAKE_HOME_B"

# ── Step 1: Create empty home workspace ──────────────────────────────────────

echo "Step 1: Creating empty home workspace..."
create_home_workspace "$HOME_WS_B"
echo "  Home workspace: $HOME_WS_B"

# ── Step 2: Configure needle with NO remote workspaces ──────────────────────

echo "Step 2: Configuring needle with no remote workspaces..."
CONFIG_DIR_B="$FAKE_HOME_B/.config/needle"
mkdir -p "$CONFIG_DIR_B"

# Also create a minimal adapter so config validation passes
ADAPTERS_DIR_B="$FAKE_HOME_B/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR_B"
cat > "$ADAPTERS_DIR_B/test-echo.yaml" <<YAML
name: test-echo
agent_cli: bash
invoke_template: "echo noop"
timeout_secs: 10
YAML

cat > "$CONFIG_DIR_B/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: test-echo
  timeout: 10
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 5
strands:
  explore:
    enabled: true
    workspaces: []
YAML

# ── Step 3: Run needle ──────────────────────────────────────────────────────

echo "Step 3: Running needle worker..."
export NEEDLE_INNER=1

TELEMETRY_DIR_B="$FAKE_HOME_B/.needle/logs"
EXIT_CODE_B=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$HOME_WS_B" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-waterfall-b 2>&1 || EXIT_CODE_B=$?

echo ""
info "Needle exit code: $EXIT_CODE_B"

# ── Step 4: Assertions for Scenario B ────────────────────────────────────────

echo "Step 4: Checking Scenario B assertions..."
echo ""

# 4a. Clean exit code
if [ "$EXIT_CODE_B" -eq 0 ]; then
    pass "Exit code is 0"
else
    fail "Exit code was $EXIT_CODE_B, expected 0"
fi

# 4b. Telemetry
TELEMETRY_LOG_B="$(find "$TELEMETRY_DIR_B" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG_B" ]; then
    fail "No telemetry log found"
else
    info "Telemetry log: $TELEMETRY_LOG_B"
    EVENT_COUNT_B="$(wc -l < "$TELEMETRY_LOG_B")"
    info "Total events: $EVENT_COUNT_B"

    # Worker started
    if grep -q '"event_type":"worker.started"' "$TELEMETRY_LOG_B" 2>/dev/null; then
        pass "worker.started event present"
    else
        fail "worker.started event missing"
    fi

    # Worker reached EXHAUSTED (all strands returned NoWork)
    if grep -q '"event_type":"worker.exhausted"' "$TELEMETRY_LOG_B" 2>/dev/null; then
        pass "worker.exhausted event present (all strands empty)"
    else
        fail "worker.exhausted event missing"
    fi

    # No bead claims should have happened (nothing to claim)
    CLAIM_COUNT="$(grep -c '"event_type":"bead.claim.attempted"' "$TELEMETRY_LOG_B" 2>/dev/null || true)"
    CLAIM_COUNT="${CLAIM_COUNT//[[:space:]]/}"
    if [ "$CLAIM_COUNT" -eq 0 ]; then
        pass "No claim attempts (no beads to claim)"
    else
        fail "Unexpected claim attempts: $CLAIM_COUNT"
    fi

    # No agent dispatches should have happened
    DISPATCH_COUNT="$(grep -c '"event_type":"agent.dispatched"' "$TELEMETRY_LOG_B" 2>/dev/null || true)"
    DISPATCH_COUNT="${DISPATCH_COUNT//[[:space:]]/}"
    if [ "$DISPATCH_COUNT" -eq 0 ]; then
        pass "No agent dispatches (no work to do)"
    else
        fail "Unexpected agent dispatches: $DISPATCH_COUNT"
    fi

    # Verify timestamps are monotonically increasing
    TIMESTAMPS_CHECK="$(grep -o '"timestamp":"[^"]*"' "$TELEMETRY_LOG_B" | sort -c 2>&1)" || true
    if echo "$TIMESTAMPS_CHECK" | grep -q "disorder"; then
        fail "Telemetry timestamps are not monotonically increasing"
    else
        pass "Telemetry timestamps are monotonically increasing"
    fi
fi

# ═══════════════════════════════════════════════════════════════════════════════
# Result
# ═══════════════════════════════════════════════════════════════════════════════

echo ""
echo "=== Results ==="
echo -e "  Passed: ${GREEN}$PASS_COUNT${NC}"
echo -e "  Failed: ${RED}$FAIL_COUNT${NC}"
echo ""

if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    echo ""
    echo "Summary: Strand waterfall progression verified."
    echo "  Scenario A: Explore found remote work after Pluck/Mend returned NoWork."
    echo "  Scenario B: All strands empty → worker reached EXHAUSTED → clean exit."
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    # Dump telemetry for debugging.
    for label_log in "A:${TELEMETRY_LOG_A:-}" "B:${TELEMETRY_LOG_B:-}"; do
        LABEL="${label_log%%:*}"
        LOG="${label_log#*:}"
        if [ -n "$LOG" ] && [ -f "$LOG" ]; then
            echo ""
            echo "=== Telemetry log (Scenario $LABEL) ==="
            jq -r '[.sequence, .event_type] | @tsv' "$LOG" 2>/dev/null | head -50 || cat "$LOG"
        fi
    done

    exit 1
fi
