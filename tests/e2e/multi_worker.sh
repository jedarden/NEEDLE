#!/bin/bash
# E2E: Multi-worker coordination — no duplicate claims
#
# Proves that multiple needle workers operating on the same workspace never
# claim the same bead, and all beads are eventually processed exactly once.
#
# Strategy:
#   - Create 10 beads
#   - Launch 3 workers against the same workspace (each TMUX=fake, --count 1)
#   - After all workers exhaust, verify all 10 beads are CLOSED and each was
#     claimed exactly once (via telemetry aggregation)
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

echo "=== E2E: Multi-Worker Coordination — No Duplicate Claims ==="
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
MARKERS_DIR="$WORKSPACE/markers"

WORKER_PIDS=()

cleanup() {
    # Kill any still-running workers.
    for pid in "${WORKER_PIDS[@]:-}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
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

# ── Step 2: Create 10 test beads ─────────────────────────────────────────────

echo "Step 2: Creating 10 test beads..."
BEAD_IDS=()
for i in $(seq 1 10); do
    BEAD_ID=""
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
        --title "E2E multi-worker task $i" \
        --description "Multi-worker coordination test bead #$i" \
        --silent 2>/dev/null)" || {
        # Retry once after sync (FrankenSQLite WAL race).
        (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
        BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create \
            --title "E2E multi-worker task $i" \
            --description "Multi-worker coordination test bead #$i" \
            --silent)"
    }
    BEAD_IDS+=("$BEAD_ID")
    echo "  Bead $i: $BEAD_ID"
done
echo "  Created ${#BEAD_IDS[@]} beads."

# ── Step 3: Create fast agent adapter ────────────────────────────────────────
#
# The agent creates a per-bead marker file and closes the bead.
# Marker files: workspace/markers/<bead_id>
# After the run, count markers == number of completions.
# If a bead was claimed twice, its marker file would exist (created twice),
# but we can detect duplicates via telemetry instead.

echo "Step 3: Creating multi-worker-test adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"
mkdir -p "$MARKERS_DIR"

cat > "$ADAPTERS_DIR/multi-worker-test.yaml" <<YAML
name: multi-worker-test
agent_cli: bash
invoke_template: "cd {workspace} && mkdir -p markers && touch markers/{bead_id} && $BR_BIN close {bead_id} --reason 'E2E multi-worker test'"
timeout_secs: 15
YAML

# ── Step 4: Configure needle ─────────────────────────────────────────────────

echo "Step 4: Configuring needle..."
CONFIG_DIR="$HOME/.config/needle"
mkdir -p "$CONFIG_DIR"

cat > "$CONFIG_DIR/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 3
agent:
  default: multi-worker-test
  timeout: 15
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 10
YAML

# ── Step 5: Launch 3 concurrent workers ──────────────────────────────────────

echo "Step 5: Launching 3 concurrent workers..."

export TMUX=fake

TELEMETRY_DIR="$HOME/.needle/logs"
WORKER_NAMES=("alpha" "bravo" "charlie")

for worker_name in "${WORKER_NAMES[@]}"; do
    "$NEEDLE_BIN" run \
        --workspace "$WORKSPACE" \
        --agent multi-worker-test \
        --count 1 \
        --identifier "$worker_name" 2>/dev/null &
    pid=$!
    WORKER_PIDS+=("$pid")
    echo "  Worker '$worker_name' PID: $pid"
done

# ── Step 6: Wait for all workers to finish ────────────────────────────────────

echo "Step 6: Waiting for workers to finish (max 60s)..."

EXIT_CODES=()
for i in "${!WORKER_PIDS[@]}"; do
    pid="${WORKER_PIDS[$i]}"
    name="${WORKER_NAMES[$i]}"
    EXIT_CODE=0
    wait "$pid" || EXIT_CODE=$?
    EXIT_CODES+=("$EXIT_CODE")
    echo "  Worker '$name' (PID $pid) exited with code $EXIT_CODE"
done

echo ""

# ── Step 7: Assertions ────────────────────────────────────────────────────────

echo "Step 7: Checking assertions..."
PASS=true

# 7a. All workers exited cleanly.
for i in "${!WORKER_NAMES[@]}"; do
    name="${WORKER_NAMES[$i]}"
    code="${EXIT_CODES[$i]}"
    if [ "$code" -eq 0 ]; then
        pass "Worker '$name' exited cleanly (exit code 0)"
    else
        fail "Worker '$name' exited with code $code"
    fi
done

# 7b. All 10 beads are CLOSED.
CLOSED_COUNT=0
for bead_id in "${BEAD_IDS[@]}"; do
    STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$bead_id" 2>/dev/null | head -1 || echo "ERROR")"
    if echo "$STATUS" | grep -qi "CLOSED\|✓"; then
        CLOSED_COUNT=$((CLOSED_COUNT + 1))
    else
        fail "Bead $bead_id not closed: $STATUS"
    fi
done

if [ "$CLOSED_COUNT" -eq 10 ]; then
    pass "All 10 beads are CLOSED"
else
    fail "Only $CLOSED_COUNT/10 beads are CLOSED"
fi

# 7c. Marker files: one per bead, exactly 10 total.
MARKER_COUNT="$(find "$MARKERS_DIR" -maxdepth 1 -type f 2>/dev/null | wc -l)"
info "Marker files: $MARKER_COUNT"
if [ "$MARKER_COUNT" -eq 10 ]; then
    pass "Exactly 10 marker files created (one per bead)"
else
    fail "Expected 10 marker files, found $MARKER_COUNT"
    info "Marker files present:"
    find "$MARKERS_DIR" -maxdepth 1 -type f 2>/dev/null | sort | sed 's/^/    /'
fi

# 7d. Telemetry: aggregate bead.claim.succeeded across all worker logs.
TOTAL_CLAIMS=0
for worker_name in "${WORKER_NAMES[@]}"; do
    WORKER_LOG="$(find "$TELEMETRY_DIR" -name "${worker_name}-*.jsonl" 2>/dev/null | head -1 || echo "")"
    if [ -n "$WORKER_LOG" ] && [ -f "$WORKER_LOG" ]; then
        WORKER_CLAIMS="$(grep -c '"event_type":"bead.claim.succeeded"' "$WORKER_LOG" 2>/dev/null || true)"
        WORKER_CLAIMS="${WORKER_CLAIMS:-0}"
        WORKER_CLAIMS="${WORKER_CLAIMS//[[:space:]]/}"
        TOTAL_CLAIMS=$((TOTAL_CLAIMS + WORKER_CLAIMS))
        info "Worker '$worker_name': $WORKER_CLAIMS bead.claim.succeeded events"
    else
        info "Worker '$worker_name': no telemetry log found"
    fi
done

if [ "$TOTAL_CLAIMS" -eq 10 ]; then
    pass "Total bead.claim.succeeded events across all workers == 10 (no duplicates)"
elif [ "$TOTAL_CLAIMS" -gt 10 ]; then
    fail "Total bead.claim.succeeded == $TOTAL_CLAIMS > 10 — duplicate claims detected!"
else
    fail "Total bead.claim.succeeded == $TOTAL_CLAIMS < 10 — some beads were not claimed"
fi

# 7e. Each worker emitted worker.exhausted (all reached terminal state cleanly).
for worker_name in "${WORKER_NAMES[@]}"; do
    WORKER_LOG="$(find "$TELEMETRY_DIR" -name "${worker_name}-*.jsonl" 2>/dev/null | head -1 || echo "")"
    if [ -n "$WORKER_LOG" ] && [ -f "$WORKER_LOG" ]; then
        if grep -q '"event_type":"worker.exhausted"' "$WORKER_LOG" 2>/dev/null; then
            pass "Worker '$worker_name' reached EXHAUSTED state"
        else
            fail "Worker '$worker_name' did not reach EXHAUSTED state"
        fi
    else
        fail "Worker '$worker_name' has no telemetry log"
    fi
done

# ── Result ─────────────────────────────────────────────────────────────────────

echo ""
if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    # Dump telemetry for debugging.
    for worker_name in "${WORKER_NAMES[@]}"; do
        WORKER_LOG="$(find "$TELEMETRY_DIR" -name "${worker_name}-*.jsonl" 2>/dev/null | head -1 || echo "")"
        if [ -n "$WORKER_LOG" ] && [ -f "$WORKER_LOG" ]; then
            echo ""
            echo "=== Telemetry log: $worker_name ==="
            cat "$WORKER_LOG" | python3 -m json.tool --no-ensure-ascii 2>/dev/null \
                || cat "$WORKER_LOG"
        fi
    done

    # Dump workspace bead state.
    echo ""
    echo "=== Workspace bead state ==="
    for bead_id in "${BEAD_IDS[@]}"; do
        cd "$WORKSPACE" && "$BR_BIN" show "$bead_id" 2>/dev/null | head -2 || true
    done

    exit 1
fi
