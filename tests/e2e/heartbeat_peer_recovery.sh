#!/bin/bash
# E2E: Heartbeat and peer monitoring — stale worker recovery
#
# Tests that the Mend strand detects a crashed worker's stale heartbeat,
# releases its claimed beads, and cleans up the worker registry entry.
# A second worker run then picks up and processes the released bead.
#
# Strategy (two-phase):
#   Phase 1: Worker-A processes the unclaimed bead (bead-2), then mend detects
#            the dead worker's stale heartbeat and releases bead-1 back to OPEN.
#   Phase 2: Worker-B starts, finds bead-1 (now OPEN), claims and processes it.
#
# Dependencies: br (beads_rust CLI), needle binary, jq

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── Color helpers ──────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

PASS=true
PASS_COUNT=0
FAIL_COUNT=0

pass() { echo -e "  ${GREEN}PASS${NC}: $1"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo -e "  ${RED}FAIL${NC}: $1"; FAIL_COUNT=$((FAIL_COUNT + 1)); PASS=false; }
info() { echo -e "  ${YELLOW}INFO${NC}: $1"; }

# ── Build needle ───────────────────────────────────────────────────────────────

echo "=== E2E: Heartbeat and Peer Monitoring — Stale Worker Recovery ==="
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

# Verify jq is available.
if ! command -v jq &>/dev/null; then
    echo "FATAL: jq is required but not found"
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

# ── Step 2: Create 2 test beads ──────────────────────────────────────────────

echo "Step 2: Creating 2 test beads..."

create_bead() {
    local title="$1"
    local bead_id
    bead_id="$(cd "$WORKSPACE" && "$BR_BIN" create --title "$title" \
        --description "Create a file called DONE in the workspace root" \
        --silent 2>/dev/null)" || {
        # Retry once after sync (FrankenSQLite WAL race).
        (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
        bead_id="$(cd "$WORKSPACE" && "$BR_BIN" create --title "$title" \
            --description "Create a file called DONE in the workspace root" \
            --silent)"
    }
    echo "$bead_id"
}

BEAD_1="$(create_bead "Bead 1: file for peer recovery test")"
BEAD_2="$(create_bead "Bead 2: file for peer recovery test")"
echo "  Bead 1: $BEAD_1"
echo "  Bead 2: $BEAD_2"

# ── Step 3: Simulate a crashed worker ────────────────────────────────────────
#
# 1. Set bead-1 to in_progress with the dead worker's assignee
# 2. Write a fake heartbeat file with stale timestamp and dead PID
# 3. Write a fake worker registry entry

echo "Step 3: Simulating a crashed worker..."

DEAD_WORKER_ID="dead-worker-e2e"
DEAD_PID=99999999  # Non-existent PID

# 3a. Set bead-1 to in_progress as the dead worker.
(cd "$WORKSPACE" && "$BR_BIN" update "$BEAD_1" --status in_progress --assignee "$DEAD_WORKER_ID" 2>&1) || {
    info "br update --status in_progress failed"
}
echo "  Set $BEAD_1 to in_progress, assigned to $DEAD_WORKER_ID"

# 3b. Write fake heartbeat file (stale: 10 minutes ago).
HEARTBEAT_DIR="$HOME/.needle/state/heartbeats"
mkdir -p "$HEARTBEAT_DIR"

# Always use python3 for reliable RFC3339 timestamps (date's %N is not portable).
STALE_TIMESTAMP="$(python3 -c "from datetime import datetime, timedelta, timezone; print((datetime.now(timezone.utc) - timedelta(minutes=10)).strftime('%Y-%m-%dT%H:%M:%S.%fZ'))")"
STARTED_TIMESTAMP="$(python3 -c "from datetime import datetime, timedelta, timezone; print((datetime.now(timezone.utc) - timedelta(hours=1)).strftime('%Y-%m-%dT%H:%M:%S.%fZ'))")"

# WorkerState serializes as SCREAMING_SNAKE_CASE via serde.
cat > "$HEARTBEAT_DIR/$DEAD_WORKER_ID.json" <<JSON
{
    "worker_id": "$DEAD_WORKER_ID",
    "pid": $DEAD_PID,
    "state": "EXECUTING",
    "current_bead": "$BEAD_1",
    "workspace": "$WORKSPACE",
    "last_heartbeat": "$STALE_TIMESTAMP",
    "started_at": "$STARTED_TIMESTAMP",
    "beads_processed": 0,
    "session": "$DEAD_WORKER_ID"
}
JSON

echo "  Wrote stale heartbeat: $HEARTBEAT_DIR/$DEAD_WORKER_ID.json"
echo "  Stale timestamp: $STALE_TIMESTAMP"

# 3c. Write fake worker registry entry.
REGISTRY_DIR="$HOME/.needle/state"
mkdir -p "$REGISTRY_DIR"

NOW_TIMESTAMP="$(python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%fZ'))")"

cat > "$REGISTRY_DIR/workers.json" <<JSON
{
    "workers": [
        {
            "id": "$DEAD_WORKER_ID",
            "pid": $DEAD_PID,
            "workspace": "$WORKSPACE",
            "agent": "test-echo",
            "model": null,
            "provider": null,
            "started_at": "$STARTED_TIMESTAMP",
            "beads_processed": 0
        }
    ],
    "updated_at": "$NOW_TIMESTAMP"
}
JSON

echo "  Wrote registry entry: $REGISTRY_DIR/workers.json"

# ── Step 4: Create test-echo agent adapter ────────────────────────────────────

echo "Step 4: Creating test-echo adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/test-echo.yaml" <<YAML
name: test-echo
agent_cli: bash
invoke_template: "cd {workspace} && echo done > DONE-{bead_id} && $BR_BIN close {bead_id} --reason 'E2E peer recovery test completed'"
timeout_secs: 30
YAML

# ── Step 5: Configure needle ─────────────────────────────────────────────────

echo "Step 5: Configuring needle..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 1
agent:
  default: test-echo
  timeout: 30
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 5
YAML

# ── Phase 1: Run worker A — processes bead-2 and detects stale peer ──────────

echo ""
echo "=== Phase 1: Worker A — process bead-2, detect stale peer ==="
echo ""

export TMUX=fake
TELEMETRY_DIR="$HOME/.needle/logs"

EXIT_CODE_A=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-peer-worker-a 2>&1 || EXIT_CODE_A=$?

echo ""

# ── Phase 1 Assertions ───────────────────────────────────────────────────────

echo "Phase 1 assertions..."

# P1-a. Exit code 0
if [ "$EXIT_CODE_A" -eq 0 ]; then
    pass "Worker A exit code is 0"
else
    fail "Worker A exit code was $EXIT_CODE_A, expected 0"
fi

# P1-b. Bead-2 should be closed (worker A processed it).
BEAD2_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_2" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD2_STATUS" | grep -qi "closed\|✓"; then
    pass "Bead $BEAD_2 is closed (processed by worker A)"
else
    fail "Bead $BEAD_2 not closed: $BEAD2_STATUS"
fi

# P1-c. Bead-1 should now be OPEN (released by mend from the dead worker).
BEAD1_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_1" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD1_STATUS" | grep -qi "open\|○"; then
    pass "Bead $BEAD_1 is open (released by mend from dead worker)"
else
    fail "Bead $BEAD_1 not open: $BEAD1_STATUS"
fi

# P1-d. DONE file exists for bead-2.
if [ -f "$WORKSPACE/DONE-$BEAD_2" ]; then
    pass "DONE-$BEAD_2 file exists"
else
    fail "DONE-$BEAD_2 file does not exist"
fi

# P1-e. Stale heartbeat file should be removed by Mend.
if [ -f "$HEARTBEAT_DIR/$DEAD_WORKER_ID.json" ]; then
    fail "Stale heartbeat file still exists for $DEAD_WORKER_ID"
else
    pass "Stale heartbeat file removed for $DEAD_WORKER_ID"
fi

# P1-f. Dead worker should be deregistered.
if [ -f "$REGISTRY_DIR/workers.json" ]; then
    DEAD_IN_REGISTRY="$(jq -r ".workers[] | select(.id == \"$DEAD_WORKER_ID\") | .id" "$REGISTRY_DIR/workers.json" 2>/dev/null || echo "")"
    if [ -z "$DEAD_IN_REGISTRY" ]; then
        pass "Dead worker $DEAD_WORKER_ID deregistered from registry"
    else
        fail "Dead worker $DEAD_WORKER_ID still in registry"
    fi
else
    pass "Registry file doesn't exist (cleaned up)"
fi

# P1-g. Telemetry events from Worker A.
TELEMETRY_LOG_A="$(find "$TELEMETRY_DIR" -name "*worker-a*" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG_A" ]; then
    fail "No telemetry log found for worker A"
else
    info "Worker A telemetry: $TELEMETRY_LOG_A"

    # peer.crashed event (mend detected and released bead from crashed worker).
    if grep -q '"event_type":"peer.crashed"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        pass "Worker A: peer.crashed event emitted"

        CRASHED_EVENT="$(grep '"event_type":"peer.crashed"' "$TELEMETRY_LOG_A" | head -1)"
        if echo "$CRASHED_EVENT" | grep -q "$BEAD_1"; then
            pass "peer.crashed references bead $BEAD_1"
        else
            fail "peer.crashed does not reference bead $BEAD_1"
            info "Event: $CRASHED_EVENT"
        fi

        if echo "$CRASHED_EVENT" | grep -q "$DEAD_WORKER_ID"; then
            pass "peer.crashed references dead worker $DEAD_WORKER_ID"
        else
            fail "peer.crashed does not reference $DEAD_WORKER_ID"
            info "Event: $CRASHED_EVENT"
        fi
    else
        fail "Worker A: peer.crashed event missing"
    fi

    # mend.cycle_summary with beads_released > 0.
    if grep -q '"event_type":"mend.cycle_summary"' "$TELEMETRY_LOG_A" 2>/dev/null; then
        MEND_SUMMARY="$(grep '"event_type":"mend.cycle_summary"' "$TELEMETRY_LOG_A" | head -1)"
        if echo "$MEND_SUMMARY" | jq -e '.data.beads_released > 0' &>/dev/null; then
            pass "mend.cycle_summary shows beads_released > 0"
        else
            fail "mend.cycle_summary shows beads_released = 0"
            info "Summary: $MEND_SUMMARY"
        fi
    else
        fail "Worker A: mend.cycle_summary missing"
    fi
fi

# ── Phase 2: Run worker B — picks up released bead-1 ────────────────────────

echo ""
echo "=== Phase 2: Worker B — pick up released bead-1 ==="
echo ""

EXIT_CODE_B=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-peer-worker-b 2>&1 || EXIT_CODE_B=$?

echo ""

# ── Phase 2 Assertions ───────────────────────────────────────────────────────

echo "Phase 2 assertions..."

# P2-a. Exit code 0.
if [ "$EXIT_CODE_B" -eq 0 ]; then
    pass "Worker B exit code is 0"
else
    fail "Worker B exit code was $EXIT_CODE_B, expected 0"
fi

# P2-b. Bead-1 should now be closed (processed by worker B).
BEAD1_FINAL="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_1" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD1_FINAL" | grep -qi "closed\|✓"; then
    pass "Bead $BEAD_1 is closed (processed by worker B after mend recovery)"
else
    fail "Bead $BEAD_1 not closed after worker B: $BEAD1_FINAL"
fi

# P2-c. DONE file exists for bead-1.
if [ -f "$WORKSPACE/DONE-$BEAD_1" ]; then
    pass "DONE-$BEAD_1 file exists"
else
    fail "DONE-$BEAD_1 file does not exist"
fi

# P2-d. Both beads are now closed.
ALL_CLOSED=true
for BEAD_ID in "$BEAD_1" "$BEAD_2"; do
    STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
    if ! echo "$STATUS" | grep -qi "closed\|✓"; then
        ALL_CLOSED=false
    fi
done
if [ "$ALL_CLOSED" = true ]; then
    pass "All beads are closed — full recovery successful"
else
    fail "Not all beads are closed"
fi

# P2-e. Worker B telemetry contains expected lifecycle events.
TELEMETRY_LOG_B="$(find "$TELEMETRY_DIR" -name "*worker-b*" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG_B" ]; then
    fail "No telemetry log found for worker B"
else
    info "Worker B telemetry: $TELEMETRY_LOG_B"

    EXPECTED_EVENTS=(
        "worker.started"
        "bead.claim.attempted"
        "bead.claim.succeeded"
        "agent.dispatched"
        "agent.completed"
        "outcome.classified"
        "outcome.handled"
        "bead.completed"
    )

    for event in "${EXPECTED_EVENTS[@]}"; do
        if grep -q "\"event_type\":\"$event\"" "$TELEMETRY_LOG_B" 2>/dev/null; then
            pass "Worker B: $event"
        else
            fail "Worker B missing: $event"
        fi
    done
fi

# ── Result ─────────────────────────────────────────────────────────────────────

echo ""
echo "=== Results ==="
echo -e "  Passed: ${GREEN}$PASS_COUNT${NC}"
echo -e "  Failed: ${RED}$FAIL_COUNT${NC}"
echo ""

if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    # Dump telemetry for debugging.
    for LOG in "$TELEMETRY_LOG_A" "${TELEMETRY_LOG_B:-}"; do
        if [ -n "$LOG" ] && [ -f "$LOG" ]; then
            echo ""
            echo "=== Telemetry: $(basename "$LOG") ==="
            jq -r '[.sequence, .event_type] | @tsv' "$LOG" 2>/dev/null || cat "$LOG"
        fi
    done

    # Dump workspace contents.
    echo ""
    echo "=== Workspace contents ==="
    ls -la "$WORKSPACE" 2>/dev/null || true

    # Dump beads status.
    echo ""
    echo "=== Bead status ==="
    (cd "$WORKSPACE" && "$BR_BIN" list 2>/dev/null) || true

    exit 1
fi
