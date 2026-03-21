#!/bin/bash
# E2E: Single worker full bead lifecycle
#
# Tests the complete flow: claim -> build prompt -> dispatch -> agent closes bead -> worker exits.
# Uses real br CLI and needle binary -- no mocks.
#
# Dependencies: br (beads_rust CLI), needle binary (built from this repo)
#
# Expected telemetry event sequence:
#   worker.started -> worker.state_transition (multiple) -> bead.claim.attempted ->
#   bead.claim.succeeded -> agent.dispatched -> agent.completed -> outcome.classified ->
#   outcome.handled -> effort.recorded -> worker.exhausted -> worker.stopped

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

echo "=== E2E: Single Worker Full Bead Lifecycle ==="
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

# Save real HOME for br discovery, then isolate.
REAL_HOME="$HOME"
export HOME="$FAKE_HOME"
mkdir -p "$HOME"

# Ensure br is still discoverable via absolute path.
# The needle binary uses which::which(br) which searches PATH.
# PATH already includes the real ~/.local/bin from the shell profile.

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
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Create a file called DONE" \
    --description "Create a file called DONE in the workspace root" \
    --silent 2>/dev/null)" || {
    # Retry once after sync (FrankenSQLite WAL race).
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Create a file called DONE" \
        --description "Create a file called DONE in the workspace root" \
        --silent)"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create test-echo agent adapter ────────────────────────────────────

echo "Step 3: Creating test-echo adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/test-echo.yaml" <<YAML
name: test-echo
agent_cli: bash
invoke_template: "cd {workspace} && echo done > DONE && $BR_BIN close {bead_id} --reason 'E2E test completed'"
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
  default: test-echo
  timeout: 30
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 5
YAML

# ── Step 5: Run needle ────────────────────────────────────────────────────────

echo "Step 5: Running needle worker..."

# TMUX=fake makes needle think it's inside tmux, so it runs the worker directly
# instead of launching a tmux session.
export TMUX=fake

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-test 2>&1 || EXIT_CODE=$?

echo ""

# ── Step 6: Assertions ────────────────────────────────────────────────────────

echo "Step 6: Checking assertions..."
PASS=true

# 6a. Exit code
if [ "$EXIT_CODE" -eq 0 ]; then
    pass "Exit code is 0"
else
    fail "Exit code was $EXIT_CODE, expected 0"
fi

# 6b. Bead is closed
BEAD_STATUS="$(cd "$WORKSPACE" && "$BR_BIN" show "$BEAD_ID" 2>/dev/null | head -1 || echo "ERROR")"
if echo "$BEAD_STATUS" | grep -qi "closed\|CLOSED\|✓"; then
    pass "Bead $BEAD_ID is closed"
else
    fail "Bead $BEAD_ID not closed: $BEAD_STATUS"
fi

# 6c. DONE file exists
if [ -f "$WORKSPACE/DONE" ]; then
    pass "DONE file exists"
else
    fail "DONE file does not exist in $WORKSPACE"
fi

# 6d. Telemetry events
TELEMETRY_LOG="$(find "$TELEMETRY_DIR" -name "*.jsonl" 2>/dev/null | head -1 || echo "")"
if [ -z "$TELEMETRY_LOG" ]; then
    fail "No telemetry log found in $TELEMETRY_DIR"
else
    info "Telemetry log: $TELEMETRY_LOG"
    EVENT_COUNT="$(wc -l < "$TELEMETRY_LOG")"
    info "Total events: $EVENT_COUNT"

    # Core lifecycle events that MUST appear.
    EXPECTED_EVENTS=(
        "worker.started"
        "worker.state_transition"
        "bead.claim.attempted"
        "bead.claim.succeeded"
        "agent.dispatched"
        "agent.completed"
        "outcome.classified"
        "outcome.handled"
        "worker.exhausted"
    )

    for event in "${EXPECTED_EVENTS[@]}"; do
        if grep -q "\"event_type\":\"$event\"" "$TELEMETRY_LOG" 2>/dev/null; then
            pass "Telemetry contains $event"
        else
            fail "Telemetry missing $event"
        fi
    done

    # Verify timestamps are monotonically increasing.
    TIMESTAMPS="$(grep -o '"timestamp":"[^"]*"' "$TELEMETRY_LOG" | sort -c 2>&1)" || true
    if echo "$TIMESTAMPS" | grep -q "disorder"; then
        fail "Telemetry timestamps are not monotonically increasing"
    else
        pass "Telemetry timestamps are monotonically increasing"
    fi

    # Verify sequence numbers are present.
    if grep -q '"sequence":' "$TELEMETRY_LOG" 2>/dev/null; then
        pass "Telemetry events have sequence numbers"
    else
        fail "Telemetry events missing sequence numbers"
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
    echo "=== Workspace contents ==="
    ls -la "$WORKSPACE" 2>/dev/null || true

    exit 1
fi
