#!/usr/bin/env bash
# Integration tests: FABRIC forwarding → Dashboard server end-to-end
#
# Tests the full pipeline:
#   fabric.sh (_needle_fabric_forward_event / _needle_fabric_parse_stream)
#     → POST /ingest or POST /ingest/batch
#       → dashboard server buffers event
#         → appears in /api/events and /api/summary

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_ROOT="$(dirname "$SCRIPT_DIR")"
SERVER_SCRIPT="$NEEDLE_ROOT/src/dashboard/server.py"

TEST_PORT=17845
SERVER_PID=""

pass=0
fail=0

_pass() { echo "PASS: $1"; pass=$((pass + 1)); }
_fail() { echo "FAIL: $1"; fail=$((fail + 1)); }

_start_server() {
    python3 "$SERVER_SCRIPT" --port "$TEST_PORT" 2>/dev/null &
    SERVER_PID=$!
    local retries=20
    while [[ $retries -gt 0 ]]; do
        if curl -sf --max-time 1 "http://localhost:$TEST_PORT/health" &>/dev/null; then
            return 0
        fi
        sleep 0.2
        retries=$((retries - 1))
    done
    echo "ERROR: Server did not start in time" >&2
    return 1
}

_stop_server() {
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null
        wait "$SERVER_PID" 2>/dev/null || true
        SERVER_PID=""
    fi
}

trap '_stop_server' EXIT

echo "=== FABRIC → Dashboard Integration Tests ==="
echo ""

# Prerequisites
if ! command -v python3 &>/dev/null; then
    echo "SKIP: python3 not available"
    exit 0
fi
if [[ ! -f "$SERVER_SCRIPT" ]]; then
    echo "FAIL: Server script not found: $SERVER_SCRIPT"
    exit 1
fi

# Source FABRIC module
source "$NEEDLE_ROOT/src/lib/output.sh"
source "$NEEDLE_ROOT/src/telemetry/fabric.sh"

# Point FABRIC at the test server
export FABRIC_ENDPOINT="http://localhost:$TEST_PORT/ingest"

echo "Starting dashboard server on port $TEST_PORT..."
if ! _start_server; then
    echo "FAIL: Could not start dashboard server"
    exit 1
fi
echo "Server started (PID: $SERVER_PID)"
echo ""

# ---------------------------------------------------------------------------
# Test 1: _needle_fabric_forward_event → /ingest
# ---------------------------------------------------------------------------
echo "Test 1: _needle_fabric_forward_event delivers event to dashboard"
_needle_fabric_forward_event \
    '{"type":"bead.claimed","ts":"2026-03-15T12:00:00.000Z","worker":"fabric-test-worker","data":{"bead_id":"nd-fabric-integ-1"}}'
sleep 1  # allow async curl to complete

events=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/events" 2>/dev/null || echo "")
if echo "$events" | python3 -c "
import sys, json
d = json.load(sys.stdin)
evs = d.get('events', [])
found = any(
    e.get('type') == 'bead.claimed'
    and e.get('data', {}).get('bead_id') == 'nd-fabric-integ-1'
    for e in evs
)
assert found, f'event not found; types={[e.get(\"type\") for e in evs]}'
" 2>/dev/null; then
    _pass "_needle_fabric_forward_event event appears in /api/events"
else
    _fail "_needle_fabric_forward_event event not received (got: ${events:0:200})"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 2: _needle_fabric_parse_stream → multiple events forwarded
# ---------------------------------------------------------------------------
echo "Test 2: _needle_fabric_parse_stream forwards all stream events"
STREAM_FILE=$(mktemp)
cat > "$STREAM_FILE" <<'EOF'
{"type":"bead.claimed","ts":"2026-03-15T12:01:00.000Z","worker":"stream-tester","data":{"bead_id":"nd-stream-bead"}}
{"type":"tool_use","ts":"2026-03-15T12:01:01.000Z","worker":"stream-tester","data":{"tool":"Bash","command":"echo hello"}}
{"type":"bead.completed","ts":"2026-03-15T12:01:02.000Z","worker":"stream-tester","data":{"bead_id":"nd-stream-bead"}}
EOF
_needle_fabric_parse_stream "$STREAM_FILE"
rm -f "$STREAM_FILE"
sleep 1  # allow async curls to complete

events2=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/events" 2>/dev/null || echo "")
if echo "$events2" | python3 -c "
import sys, json
d = json.load(sys.stdin)
evs = d.get('events', [])
types = {e.get('type') for e in evs}
assert 'bead.completed' in types, f'bead.completed missing from {types}'
assert 'tool_use' in types, f'tool_use missing from {types}'
assert any(e.get('data', {}).get('bead_id') == 'nd-stream-bead' for e in evs), \
    'nd-stream-bead not found in events'
" 2>/dev/null; then
    _pass "_needle_fabric_parse_stream events all appear in /api/events"
else
    _fail "_needle_fabric_parse_stream events missing (got: ${events2:0:300})"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 3: forwarded worker appears in summary
# ---------------------------------------------------------------------------
echo "Test 3: Forwarded bead.claimed event populates worker in summary"
summary=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary" | python3 -c "
import sys, json
d = json.load(sys.stdin)
workers = d.get('workers_all', {})
assert 'fabric-test-worker' in workers, \
    f'fabric-test-worker missing from workers_all: {list(workers.keys())}'
" 2>/dev/null; then
    _pass "Forwarded worker appears in /api/summary workers_all"
else
    _fail "Forwarded worker missing from summary (got: ${summary:0:200})"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 4: bead.completed via FABRIC increments throughput history
# ---------------------------------------------------------------------------
echo "Test 4: bead.completed forwarded via FABRIC updates throughput sparkline"
NOW_TS=$(python3 -c "
from datetime import datetime, timezone
print(datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.000Z'))
")
_needle_fabric_forward_event \
    "{\"type\":\"bead.completed\",\"ts\":\"$NOW_TS\",\"worker\":\"fabric-test-worker\",\"data\":{\"bead_id\":\"nd-fabric-integ-2\"}}"
sleep 1

throughput=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/throughput" 2>/dev/null || echo "")
if echo "$throughput" | python3 -c "
import sys, json
d = json.load(sys.stdin)
total = sum(h['count'] for h in d.get('history', []))
assert total >= 1, f'expected >= 1 completion in throughput history, got total={total}'
" 2>/dev/null; then
    _pass "bead.completed via FABRIC updates throughput history"
else
    _fail "bead.completed via FABRIC not in throughput (got: ${throughput:0:200})"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 5: POST /ingest/batch — batch of events
# ---------------------------------------------------------------------------
echo "Test 5: POST /ingest/batch accepts a JSON array of events"
batch_result=$(curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest/batch" \
    -H "Content-Type: application/json" \
    -d '[
        {"type":"bead.claimed","ts":"2026-03-15T13:00:00.000Z","worker":"batch-worker","data":{"bead_id":"nd-batch-1"}},
        {"type":"bead.completed","ts":"2026-03-15T13:00:01.000Z","worker":"batch-worker","data":{"bead_id":"nd-batch-1"}},
        {"type":"result","ts":"2026-03-15T13:00:02.000Z","worker":"batch-worker","data":{"usage":{"input_tokens":500,"output_tokens":200},"cost":0.003}}
    ]' 2>/dev/null || echo "")
if echo "$batch_result" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert d.get('status') == 'ok', f'expected status=ok, got {d}'
assert d.get('count') == 3, f'expected count=3, got {d.get(\"count\")}'
" 2>/dev/null; then
    _pass "POST /ingest/batch accepts array and returns count=3"
else
    _fail "POST /ingest/batch failed (got: $batch_result)"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 6: /batch alias also works
# ---------------------------------------------------------------------------
echo "Test 6: POST /batch (alias) also accepts batch events"
alias_result=$(curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/batch" \
    -H "Content-Type: application/json" \
    -d '[{"type":"test.event","ts":"2026-03-15T13:01:00.000Z","worker":"batch-worker","data":{}}]' \
    2>/dev/null || echo "")
if echo "$alias_result" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert d.get('status') == 'ok', f'expected status=ok, got {d}'
assert d.get('count') == 1, f'expected count=1, got {d.get(\"count\")}'
" 2>/dev/null; then
    _pass "POST /batch alias works for batch ingestion"
else
    _fail "POST /batch alias failed (got: $alias_result)"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 7: batch events appear in /api/events
# ---------------------------------------------------------------------------
echo "Test 7: Batch-ingested events appear in /api/events"
events3=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/events" 2>/dev/null || echo "")
if echo "$events3" | python3 -c "
import sys, json
d = json.load(sys.stdin)
evs = d.get('events', [])
bead_ids = {e.get('data', {}).get('bead_id') for e in evs if isinstance(e.get('data'), dict)}
assert 'nd-batch-1' in bead_ids, f'nd-batch-1 not found in bead_ids={bead_ids}'
" 2>/dev/null; then
    _pass "Batch-ingested events appear in /api/events"
else
    _fail "Batch-ingested events missing from /api/events (got: ${events3:0:200})"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 8: batch with single object (graceful fallback)
# ---------------------------------------------------------------------------
echo "Test 8: POST /ingest/batch accepts single JSON object (graceful fallback)"
single_result=$(curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest/batch" \
    -H "Content-Type: application/json" \
    -d '{"type":"bead.claimed","ts":"2026-03-15T13:02:00.000Z","worker":"single-worker","data":{"bead_id":"nd-single-via-batch"}}' \
    2>/dev/null || echo "")
if echo "$single_result" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert d.get('status') == 'ok', f'expected status=ok, got {d}'
assert d.get('count') == 1, f'expected count=1, got {d.get(\"count\")}'
" 2>/dev/null; then
    _pass "POST /ingest/batch handles single object with count=1"
else
    _fail "POST /ingest/batch single-object fallback failed (got: $single_result)"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 9: batch with invalid JSON returns 400
# ---------------------------------------------------------------------------
echo "Test 9: POST /ingest/batch rejects invalid JSON"
bad_result=$(curl -s --max-time 5 -o /dev/null -w "%{http_code}" \
    -X POST "http://localhost:$TEST_PORT/ingest/batch" \
    -H "Content-Type: application/json" \
    -d 'not-json' 2>/dev/null || echo "000")
if [[ "$bad_result" == "400" ]]; then
    _pass "POST /ingest/batch returns 400 for invalid JSON"
else
    _fail "POST /ingest/batch should return 400 for invalid JSON (got: $bad_result)"
fi
echo ""

# ---------------------------------------------------------------------------
# Test 10: batch worker cost tracked in summary
# ---------------------------------------------------------------------------
echo "Test 10: Batch-ingested result event contributes to worker cost in summary"
summary2=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary2" | python3 -c "
import sys, json
d = json.load(sys.stdin)
workers = d.get('workers_all', {})
bw = workers.get('batch-worker', {})
assert bw.get('cost', 0) > 0, \
    f'expected batch-worker cost > 0, got cost={bw.get(\"cost\")}'
" 2>/dev/null; then
    _pass "Batch result event contributes to batch-worker cost in summary"
else
    _fail "Batch result event not reflected in summary worker cost (got: ${summary2:0:300})"
fi
echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "=== Results: $pass passed, $fail failed ==="
if [[ $fail -gt 0 ]]; then
    exit 1
fi
