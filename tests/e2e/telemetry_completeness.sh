#!/bin/bash
# E2E: Telemetry event completeness
#
# Validates that every state transition and outcome in a single-worker full
# bead lifecycle emits the correct telemetry event to the JSONL log.
#
# This test runs a real needle worker against a real br workspace and then
# parses the resulting JSONL log to assert:
#   1. Every required event type is present
#   2. Events are in the correct causal order
#   3. Required data fields are present per event type
#   4. Timestamps are monotonically increasing
#   5. Sequence numbers are strictly increasing
#   6. No duplicate events for single-occurrence transitions
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
    jq -c "select(.event_type == \"$1\")" "$TELEMETRY_LOG" | head -1
}

# Helper: count occurrences of an event_type.
count_event() {
    jq -c "select(.event_type == \"$1\")" "$TELEMETRY_LOG" | wc -l
}

# Helper: check if a data field exists in an event's data object.
# Usage: check_data_field "event_type" "field_name"
check_data_field() {
    local event_type="$1"
    local field="$2"
    local evt
    evt=$(first_event "$event_type")
    if [ -z "$evt" ]; then
        fail "$event_type.$field — event not found"
        return
    fi
    if echo "$evt" | jq -e ".data.$field // empty" &>/dev/null; then
        pass "$event_type has $field field"
    else
        fail "$event_type missing $field field"
    fi
}

# ── Check prerequisites ──────────────────────────────────────────────────────

echo "=== E2E: Telemetry Event Completeness ==="
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

# ── Step 2: Create test bead ────────────────────────────────────────────────

echo "Step 2: Creating test bead..."
BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Create a file called DONE" \
    --description "Create a file called DONE in the workspace root" \
    --silent 2>/dev/null)" || {
    (cd "$WORKSPACE" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
    BEAD_ID="$(cd "$WORKSPACE" && "$BR_BIN" create --title "Create a file called DONE" \
        --description "Create a file called DONE in the workspace root" \
        --silent)"
}
echo "  Bead: $BEAD_ID"

# ── Step 3: Create test-echo agent adapter ──────────────────────────────────

echo "Step 3: Creating test-echo adapter..."
ADAPTERS_DIR="$HOME/.config/needle/adapters"
mkdir -p "$ADAPTERS_DIR"

cat > "$ADAPTERS_DIR/test-echo.yaml" <<YAML
name: test-echo
agent_cli: bash
invoke_template: "cd {workspace} && echo done > DONE && $BR_BIN close {bead_id} --reason 'E2E test completed'"
timeout_secs: 30
YAML

# ── Step 4: Configure needle ────────────────────────────────────────────────

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

# ── Step 5: Run needle ──────────────────────────────────────────────────────

echo "Step 5: Running needle worker..."
export TMUX=fake

TELEMETRY_DIR="$HOME/.needle/logs"
EXIT_CODE=0
timeout 30 "$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent test-echo \
    --count 1 \
    --identifier e2e-telemetry-test 2>&1 || EXIT_CODE=$?

echo ""

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

# ── Step 7: Assert required events are present ──────────────────────────────

echo "Step 7: Checking required event types..."

REQUIRED_EVENTS=(
    "worker.started"
    "worker.state_transition"
    "bead.claim.attempted"
    "bead.claim.succeeded"
    "agent.dispatched"
    "agent.completed"
    "outcome.classified"
    "bead.completed"
    "outcome.handled"
    "effort.recorded"
    "worker.exhausted"
    "worker.stopped"
)

for event in "${REQUIRED_EVENTS[@]}"; do
    count=$(count_event "$event")
    if [ "$count" -gt 0 ]; then
        pass "Event '$event' present ($count occurrences)"
    else
        fail "Event '$event' MISSING from telemetry log"
    fi
done

echo ""

# ── Step 8: Check event ordering ────────────────────────────────────────────

echo "Step 8: Checking causal event ordering..."

# Ordered pairs: each event must appear before the next one in the log.
ORDERED_PAIRS=(
    "worker.started:bead.claim.attempted"
    "bead.claim.attempted:bead.claim.succeeded"
    "bead.claim.succeeded:agent.dispatched"
    "agent.dispatched:agent.completed"
    "agent.completed:outcome.classified"
    "outcome.classified:bead.completed"
    "bead.completed:outcome.handled"
    "outcome.handled:effort.recorded"
    "effort.recorded:worker.exhausted"
    "worker.exhausted:worker.stopped"
)

for pair in "${ORDERED_PAIRS[@]}"; do
    BEFORE="${pair%%:*}"
    AFTER="${pair##*:}"

    SEQ_BEFORE=$(first_event "$BEFORE" | jq -r '.sequence // empty')
    SEQ_AFTER=$(first_event "$AFTER" | jq -r '.sequence // empty')

    if [ -z "$SEQ_BEFORE" ] || [ -z "$SEQ_AFTER" ]; then
        fail "Cannot check order $BEFORE -> $AFTER (one or both events missing)"
    elif [ "$SEQ_BEFORE" -lt "$SEQ_AFTER" ]; then
        pass "Order: $BEFORE (seq $SEQ_BEFORE) -> $AFTER (seq $SEQ_AFTER)"
    else
        fail "Order violated: $BEFORE (seq $SEQ_BEFORE) should precede $AFTER (seq $SEQ_AFTER)"
    fi
done

echo ""

# ── Step 9: Check state transitions ─────────────────────────────────────────

echo "Step 9: Checking state transition sequence..."

# Extract from/to pairs from state transition events (states are uppercase in JSONL).
TRANSITIONS=$(jq -c 'select(.event_type == "worker.state_transition") | "\(.data.from) -> \(.data.to)"' "$TELEMETRY_LOG" | tr -d '"')

EXPECTED_TRANSITIONS=(
    "BOOTING -> SELECTING"
    "SELECTING -> CLAIMING"
    "CLAIMING -> BUILDING"
    "BUILDING -> DISPATCHING"
    "DISPATCHING -> EXECUTING"
    "EXECUTING -> HANDLING"
    "HANDLING -> LOGGING"
    "LOGGING -> SELECTING"
    "SELECTING -> EXHAUSTED"
)

for expected in "${EXPECTED_TRANSITIONS[@]}"; do
    if echo "$TRANSITIONS" | grep -qF "$expected"; then
        pass "State transition: $expected"
    else
        fail "Missing state transition: $expected"
    fi
done

echo ""

# ── Step 10: Check timestamps monotonically increasing ──────────────────────

echo "Step 10: Checking timestamp ordering..."

TIMESTAMPS=$(jq -r '.timestamp' "$TELEMETRY_LOG")
PREV_TS=""
TS_MONOTONIC=true
TS_LINE=0

while IFS= read -r ts; do
    TS_LINE=$((TS_LINE + 1))
    if [ -n "$PREV_TS" ]; then
        if [[ "$ts" < "$PREV_TS" ]]; then
            fail "Timestamp disorder at line $TS_LINE: $PREV_TS > $ts"
            TS_MONOTONIC=false
            break
        fi
    fi
    PREV_TS="$ts"
done <<< "$TIMESTAMPS"

if [ "$TS_MONOTONIC" = true ]; then
    pass "All timestamps are monotonically non-decreasing"
fi

echo ""

# ── Step 11: Check sequence numbers strictly increasing ─────────────────────

echo "Step 11: Checking sequence numbers..."

SEQUENCES=$(jq -r '.sequence' "$TELEMETRY_LOG")
PREV_SEQ=-1
SEQ_STRICT=true
SEQ_LINE=0

while IFS= read -r seq; do
    SEQ_LINE=$((SEQ_LINE + 1))
    if [ "$seq" -le "$PREV_SEQ" ]; then
        fail "Sequence number not strictly increasing at line $SEQ_LINE: $PREV_SEQ >= $seq"
        SEQ_STRICT=false
        break
    fi
    PREV_SEQ="$seq"
done <<< "$SEQUENCES"

if [ "$SEQ_STRICT" = true ]; then
    pass "All sequence numbers are strictly increasing"
fi

echo ""

# ── Step 12: Check no duplicate single-occurrence events ─────────────────────

echo "Step 12: Checking no duplicate single-occurrence events..."

SINGLE_EVENTS=(
    "worker.started"
    "worker.exhausted"
    "worker.stopped"
    "bead.claim.attempted"
    "bead.claim.succeeded"
    "bead.completed"
    "outcome.classified"
    "outcome.handled"
    "effort.recorded"
)

for event in "${SINGLE_EVENTS[@]}"; do
    count=$(count_event "$event")
    if [ "$count" -eq 1 ]; then
        pass "Exactly 1 occurrence of '$event'"
    elif [ "$count" -eq 0 ]; then
        fail "Expected 1 occurrence of '$event', found 0"
    else
        fail "Expected 1 occurrence of '$event', found $count (duplicate!)"
    fi
done

echo ""

# ── Step 13: Check required data fields per event type ──────────────────────

echo "Step 13: Checking required data fields..."

# worker.started: worker_name, version
check_data_field "worker.started" "worker_name"
check_data_field "worker.started" "version"

# bead.claim.attempted: bead_id, attempt
check_data_field "bead.claim.attempted" "bead_id"
check_data_field "bead.claim.attempted" "attempt"

# bead.claim.succeeded: bead_id
check_data_field "bead.claim.succeeded" "bead_id"

# agent.dispatched: bead_id, agent, prompt_len
check_data_field "agent.dispatched" "bead_id"
check_data_field "agent.dispatched" "agent"
check_data_field "agent.dispatched" "prompt_len"

# agent.completed: bead_id, exit_code, duration_ms
check_data_field "agent.completed" "bead_id"
check_data_field "agent.completed" "exit_code"
check_data_field "agent.completed" "duration_ms"

# outcome.classified: bead_id, outcome, exit_code
check_data_field "outcome.classified" "bead_id"
check_data_field "outcome.classified" "outcome"
check_data_field "outcome.classified" "exit_code"

# outcome.handled: bead_id, outcome, action
check_data_field "outcome.handled" "bead_id"
check_data_field "outcome.handled" "outcome"
check_data_field "outcome.handled" "action"

# bead.completed: bead_id
check_data_field "bead.completed" "bead_id"

# effort.recorded: bead_id, elapsed_ms, agent_name
check_data_field "effort.recorded" "bead_id"
check_data_field "effort.recorded" "elapsed_ms"
check_data_field "effort.recorded" "agent_name"

# worker.stopped: reason, beads_processed
check_data_field "worker.stopped" "reason"
check_data_field "worker.stopped" "beads_processed"

# worker.exhausted: cycle_count, last_strand_evaluated
check_data_field "worker.exhausted" "cycle_count"
check_data_field "worker.exhausted" "last_strand_evaluated"

# worker.state_transition: from, to
check_data_field "worker.state_transition" "from"
check_data_field "worker.state_transition" "to"

echo ""

# ── Step 14: Check common envelope fields on all events ─────────────────────

echo "Step 14: Checking common envelope fields..."

ALL_VALID=true
LINE=0
while IFS= read -r event; do
    LINE=$((LINE + 1))
    for field in timestamp event_type worker_id session_id sequence; do
        val=$(echo "$event" | jq -r ".$field // empty")
        if [ -z "$val" ]; then
            fail "Line $LINE missing envelope field '$field'"
            ALL_VALID=false
            break 2
        fi
    done
done < "$TELEMETRY_LOG"

if [ "$ALL_VALID" = true ]; then
    pass "All events have required envelope fields (timestamp, event_type, worker_id, session_id, sequence)"
fi

# Check session_id consistency
SESSION_IDS=$(jq -r '.session_id' "$TELEMETRY_LOG" | sort -u | wc -l)
if [ "$SESSION_IDS" -eq 1 ]; then
    pass "All events share the same session_id"
else
    fail "Multiple session_ids found ($SESSION_IDS distinct values)"
fi

# Check worker_id consistency
WORKER_IDS=$(jq -r '.worker_id' "$TELEMETRY_LOG" | sort -u | wc -l)
if [ "$WORKER_IDS" -eq 1 ]; then
    pass "All events share the same worker_id"
else
    fail "Multiple worker_ids found ($WORKER_IDS distinct values)"
fi

echo ""

# ── Step 15: Verify bead_id context on bead-scoped events ───────────────────

echo "Step 15: Checking bead_id on bead-scoped events..."

BEAD_SCOPED_EVENTS=(
    "bead.claim.attempted"
    "bead.claim.succeeded"
    "agent.dispatched"
    "agent.completed"
    "outcome.classified"
    "outcome.handled"
    "bead.completed"
    "effort.recorded"
)

for event in "${BEAD_SCOPED_EVENTS[@]}"; do
    BEAD_FIELD=$(first_event "$event" | jq -r '.bead_id // empty')
    if [ -n "$BEAD_FIELD" ]; then
        pass "$event has bead_id in envelope"
    else
        fail "$event missing bead_id in envelope"
    fi
done

echo ""

# ── Result ───────────────────────────────────────────────────────────────────

echo "=== Results ==="
echo -e "  Passed: ${GREEN}$PASS_COUNT${NC}"
echo -e "  Failed: ${RED}$FAIL_COUNT${NC}"
echo ""

if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"

    if [ -f "$TELEMETRY_LOG" ]; then
        echo ""
        echo "=== Telemetry log (event_type + sequence) ==="
        jq -r '[.sequence, .event_type] | @tsv' "$TELEMETRY_LOG" 2>/dev/null || cat "$TELEMETRY_LOG"
    fi

    exit 1
fi
