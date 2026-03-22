#!/bin/bash
# E2E: Graceful shutdown — SIGTERM during execution
#
# Tests that sending SIGTERM to a needle worker during active agent execution
# results in a clean shutdown: the bead is released (not left claimed), and the
# worker exits with state STOPPED and exit code 0.
#
# Detection strategy: the agent creates a marker file on start, then sleeps.
# The test polls for the marker to know the agent is running before sending
# SIGTERM. The telemetry log is validated AFTER the process exits (the writer
# uses BufWriter which only flushes on exit).
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

echo "=== E2E: Graceful Shutdown — SIGTERM During Execution ==="
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
MARKER_FILE="$TMPBASE/AGENT_RUNNING"

cleanup() {
    # Kill needle if still running.
    if [ -n "${NEEDLE_PID:-}" ] && kill -0 "$NEEDLE_PID" 2>/dev/null; then
        kill -9 "$NEEDLE_PID" 2>/dev/null || true
        wait "$NEEDLE_PID" 2>/dev/null || true
    fi
    rm -rf "$TMPBASE"
}
trap cleanup EXIT

# Save real HOME for br discovery, then isolate.
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

# ── Step 2: Create test bead ──────────────────────────────────────────────────

echo "Step 2: Creating test bead..."
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Long-running task for SIGTERM test" \
    --description "This bead should be released (not closed) when the worker receives SIGTERM" \
    --silent 2>/dev/null)" || {
    # Retry once after sync (FrankenSQLite WAL race).
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Long-running task for SIGTERM test" \
        --description "This bead should be released when SIGTERM arrives" \
        --silent)"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create slow agent adapter ─────────────────────────────────────────
#
# The agent creates a marker file (so the test knows it's running), then sleeps.
# We send SIGTERM to needle after detecting the marker. The agent sleeps for 3s
# — short enough for a fast test, long enough to guarantee SIGTERM arrives
# during execution.

echo "Step 3: Creating slow-agent adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/slow-agent.yaml" <<YAML
name: slow-agent
agent_cli: bash
invoke_template: "touch $MARKER_FILE && sleep 3"
timeout_secs: 30
YAML

# ── Step 4: Configure needle ─────────────────────────────────────────────────

echo "Step 4: Configuring needle..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: slow-agent
  timeout: 30
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 5
YAML

# ── Step 5: Run needle and send SIGTERM ───────────────────────────────────────

echo "Step 5: Running needle worker and sending SIGTERM..."

TELEMETRY_DIR="$HOME/.needle/logs"

# NEEDLE_INNER=1 marks this as a re-entrant inner invocation, running the worker directly.
export NEEDLE_INNER=1

# Launch needle in background.
"$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent slow-agent \
    --count 1 \
    --identifier e2e-sigterm 2>/dev/null &
NEEDLE_PID=$!
echo "  Needle PID: $NEEDLE_PID"

# Poll for the marker file to know the agent is running.
echo "  Waiting for agent to start (marker file)..."
WAITED=0
MAX_WAIT=30  # 30 x 0.5s = 15s max
while [ "$WAITED" -lt "$MAX_WAIT" ]; do
    if [ -f "$MARKER_FILE" ]; then
        break
    fi
    # Check if needle already exited (error case).
    if ! kill -0 "$NEEDLE_PID" 2>/dev/null; then
        echo "  FATAL: Needle exited before agent started"
        wait "$NEEDLE_PID" 2>/dev/null || true
        exit 1
    fi
    sleep 0.5
    WAITED=$((WAITED + 1))
done

if [ ! -f "$MARKER_FILE" ]; then
    echo "  FATAL: Agent never started (marker file not created after ${MAX_WAIT}x0.5s)"
    kill "$NEEDLE_PID" 2>/dev/null || true
    exit 1
fi

info "Agent is running — sending SIGTERM to needle (PID $NEEDLE_PID)"

# Send SIGTERM to needle (just the worker process, not the process group).
kill -TERM "$NEEDLE_PID"

# Wait for needle to exit. The agent finishes its 3s sleep, then the worker
# detects was_interrupted=true, releases the bead, and stops.
echo "  Waiting for needle to exit..."
EXIT_CODE=0
wait "$NEEDLE_PID" || EXIT_CODE=$?
echo "  Needle exited with code: $EXIT_CODE"

echo ""

# ── Step 6: Assertions ────────────────────────────────────────────────────────

echo "Step 6: Checking assertions..."
PASS=true

# 6a. Exit code should be 0 (clean shutdown).
if [ "$EXIT_CODE" -eq 0 ]; then
    pass "Exit code is 0 (clean shutdown)"
else
    fail "Exit code was $EXIT_CODE, expected 0"
fi

# 6b. Bead should NOT be closed (it was interrupted, not completed).
BEAD_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD_STATUS" | grep -qi "OPEN\|○"; then
    pass "Bead $BEAD_ID is still open (released, not closed)"
elif echo "$BEAD_STATUS" | grep -qi "CLOSED\|✓"; then
    fail "Bead $BEAD_ID was closed — should have been released (interrupted)"
else
    fail "Bead $BEAD_ID status unknown: $BEAD_STATUS"
fi

# 6c. Heartbeat file should be cleaned up.
HB_DIR="$HOME/.needle/state/heartbeats"
if [ -d "$HB_DIR" ]; then
    HB_FILES="$(find "$HB_DIR" -name "*e2e-sigterm*" 2>/dev/null | wc -l)"
    if [ "$HB_FILES" -eq 0 ]; then
        pass "Heartbeat file cleaned up"
    else
        fail "Heartbeat file still exists for e2e-sigterm worker"
    fi
else
    pass "Heartbeat directory doesn't exist (no stale files)"
fi

# 6d. Worker registry entry should be removed.
REG_DIR="$HOME/.needle/state/registry"
if [ -d "$REG_DIR" ]; then
    REG_FILES="$(find "$REG_DIR" -name "*e2e-sigterm*" 2>/dev/null | wc -l)"
    if [ "$REG_FILES" -eq 0 ]; then
        pass "Worker registry entry removed"
    else
        fail "Worker registry entry still exists for e2e-sigterm worker"
    fi
else
    pass "Registry directory doesn't exist (no stale entries)"
fi

# 6e. Telemetry events (flushed to disk on exit).
TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG" ]; then
    fail "No telemetry log found in $TELEMETRY_DIR"
else
    info "Telemetry log: $TELEMETRY_LOG"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
    info "Total events: $EVENT_COUNT"

    # Core lifecycle events for interrupted shutdown.
    EXPECTED_EVENTS=(
        "worker.started"
        "worker.state_transition"
        "bead.claim.attempted"
        "bead.claim.succeeded"
        "agent.dispatched"
        "agent.completed"
        "outcome.classified"
        "outcome.handled"
        "bead.released"
        "worker.stopped"
    )

    for event in "${EXPECTED_EVENTS[@]}"; do
        if grep -q "\"event_type\":\"$event\"" "$TELEMETRY_LOG" 2>/dev/null; then
            pass "Telemetry contains $event"
        else
            fail "Telemetry missing $event"
        fi
    done

    # Verify the outcome was classified as "interrupted".
    if grep '"event_type":"outcome.classified"' "$TELEMETRY_LOG" 2>/dev/null | grep -q '"interrupted"'; then
        pass "Outcome classified as 'interrupted'"
    else
        fail "Outcome not classified as 'interrupted'"
        info "Outcome event: $(grep '"event_type":"outcome.classified"' "$TELEMETRY_LOG" 2>/dev/null || echo 'not found')"
    fi

    # Verify the bead.released reason is "interrupted".
    if grep '"event_type":"bead.released"' "$TELEMETRY_LOG" 2>/dev/null | grep -q '"interrupted"'; then
        pass "Bead released with reason 'interrupted'"
    else
        fail "Bead not released with reason 'interrupted'"
        info "Released event: $(grep '"event_type":"bead.released"' "$TELEMETRY_LOG" 2>/dev/null || echo 'not found')"
    fi

    # The worker.stopped event should have beads_processed=0
    # (the bead was interrupted, not completed).
    if grep '"event_type":"worker.stopped"' "$TELEMETRY_LOG" 2>/dev/null | grep -q '"beads_processed":0'; then
        pass "Worker stopped with beads_processed=0 (bead was interrupted, not completed)"
    else
        fail "Worker stopped with unexpected beads_processed count"
        info "Stopped event: $(grep '"event_type":"worker.stopped"' "$TELEMETRY_LOG" 2>/dev/null || echo 'not found')"
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

    exit 1
fi
