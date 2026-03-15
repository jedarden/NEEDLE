#!/usr/bin/env bash
# Tests for the FABRIC dashboard server
# Verifies HTTP endpoints: /ingest, /stream, /api/summary, /health

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_ROOT="$(dirname "$SCRIPT_DIR")"
SERVER_SCRIPT="$NEEDLE_ROOT/src/dashboard/server.py"

TEST_PORT=17842
SERVER_PID=""

pass=0
fail=0

_pass() { echo "PASS: $1"; pass=$((pass + 1)); }
_fail() { echo "FAIL: $1"; fail=$((fail + 1)); }

_start_server() {
    python3 "$SERVER_SCRIPT" --port "$TEST_PORT" 2>/dev/null &
    SERVER_PID=$!
    # Wait for server to be ready (up to 4 seconds)
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

echo "=== FABRIC Dashboard Server Tests ==="
echo ""

# Verify Python and server script are present
if ! command -v python3 &>/dev/null; then
    echo "SKIP: python3 not available"
    exit 0
fi

if [[ ! -f "$SERVER_SCRIPT" ]]; then
    echo "FAIL: Server script not found: $SERVER_SCRIPT"
    exit 1
fi

# Start server
echo "Starting test server on port $TEST_PORT..."
if ! _start_server; then
    echo "FAIL: Could not start server"
    exit 1
fi
echo "Server started (PID: $SERVER_PID)"
echo ""

# Test 1: /health endpoint
echo "Test 1: GET /health"
health=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/health" 2>/dev/null || echo "")
if echo "$health" | grep -q '"status"'; then
    _pass "Health endpoint returns status"
else
    _fail "Health endpoint did not return status (got: $health)"
fi
echo ""

# Test 2: GET / returns dashboard HTML
echo "Test 2: GET / returns dashboard HTML"
html=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/" 2>/dev/null || echo "")
if echo "$html" | grep -q "FABRIC Dashboard"; then
    _pass "Root endpoint serves dashboard HTML"
else
    _fail "Root endpoint did not return dashboard HTML"
fi
echo ""

# Test 3: GET /api/summary returns JSON
echo "Test 3: GET /api/summary returns JSON"
summary=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'events_total' in d" 2>/dev/null; then
    _pass "Summary endpoint returns valid JSON with expected keys"
else
    _fail "Summary endpoint failed (got: $summary)"
fi
echo ""

# Test 4: POST /ingest accepts an event
echo "Test 4: POST /ingest accepts event"
ingest_result=$(curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"bead.claimed","ts":"2026-03-15T12:00:00.000Z","event":"bead.claimed","session":"test-session","worker":"test-worker","data":{"bead_id":"nd-test"}}' \
    2>/dev/null || echo "")
if echo "$ingest_result" | grep -q '"ok"'; then
    _pass "Ingest endpoint accepts event"
else
    _fail "Ingest endpoint rejected event (got: $ingest_result)"
fi
echo ""

# Test 5: POST /ingest invalid JSON returns 400
echo "Test 5: POST /ingest rejects invalid JSON"
bad_result=$(curl -s --max-time 5 -o /dev/null -w "%{http_code}" -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d 'not-json' 2>/dev/null || echo "000")
if [[ "$bad_result" == "400" ]]; then
    _pass "Ingest endpoint returns 400 for invalid JSON"
else
    _fail "Ingest endpoint should return 400 for invalid JSON (got: $bad_result)"
fi
echo ""

# Test 6: Event appears in /api/summary after ingest
echo "Test 6: Ingested event appears in summary"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"result","ts":"2026-03-15T12:00:01.000Z","worker":"test-worker","data":{"usage":{"input_tokens":100,"output_tokens":50},"cost":0.005}}' \
    &>/dev/null || true
summary2=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary2" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['events_total'] >= 2" 2>/dev/null; then
    _pass "Summary reflects ingested events"
else
    _fail "Summary did not reflect ingested events (got: $summary2)"
fi
echo ""

# Test 7: GET /api/events returns event list
echo "Test 7: GET /api/events returns events"
events_result=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/events" 2>/dev/null || echo "")
if echo "$events_result" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'events' in d and isinstance(d['events'], list)" 2>/dev/null; then
    _pass "Events endpoint returns list"
else
    _fail "Events endpoint failed (got: $events_result)"
fi
echo ""

# Test 8: Unknown route returns 404
echo "Test 8: Unknown route returns 404"
not_found=$(curl -s --max-time 5 -o /dev/null -w "%{http_code}" "http://localhost:$TEST_PORT/nonexistent" 2>/dev/null || echo "000")
if [[ "$not_found" == "404" ]]; then
    _pass "Unknown route returns 404"
else
    _fail "Unknown route should return 404 (got: $not_found)"
fi
echo ""

# Test 9: SSE /stream endpoint sends initial connection event
# -N disables curl's output buffering so we get data as it arrives
echo "Test 9: GET /stream sends SSE data"
# Use --max-time 5 to give server time to send initial events, no outer timeout
sse_output=$(curl -sfN --max-time 5 "http://localhost:$TEST_PORT/stream" 2>/dev/null || true)
if echo "$sse_output" | grep -q "^data:"; then
    _pass "SSE stream sends data: lines"
else
    _fail "SSE stream did not send expected data (got: ${sse_output:0:100})"
fi
echo ""

# Test 10: Summary includes per-worker cost and daily_budget fields
echo "Test 10: Summary includes per-worker cost breakdown and daily_budget"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"result","ts":"2026-03-15T12:00:02.000Z","worker":"cost-worker","data":{"usage":{"input_tokens":200,"output_tokens":80},"cost":0.0123}}' \
    &>/dev/null || true
sleep 0.2  # Allow event to be processed
summary3=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary3" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert 'daily_budget' in d, 'missing daily_budget'
assert 'workers_all' in d, 'missing workers_all'
workers = d.get('workers_all', {})
cw = workers.get('cost-worker', {})
assert cw.get('cost', 0) > 0, f'expected cost > 0 for cost-worker, got {cw}'
" 2>/dev/null; then
    _pass "Summary includes daily_budget and per-worker cost"
else
    _fail "Summary missing daily_budget or per-worker cost (got: ${summary3:0:200})"
fi
echo ""

# Test 11: budget.warning event appears in failures
echo "Test 11: budget.warning event appears in failures list"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"budget.warning","ts":"2026-03-15T12:00:03.000Z","worker":"cost-worker","data":{"message":"Daily budget 80% consumed","spent":40.0,"budget":50.0}}' \
    &>/dev/null || true
sleep 0.2  # Allow event to be processed
summary4=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary4" | python3 -c "
import sys, json
d = json.load(sys.stdin)
failures = d.get('failures', [])
budget_warns = [f for f in failures if f.get('type') == 'budget_warning']
assert len(budget_warns) > 0, f'no budget_warning in failures: {failures}'
" 2>/dev/null; then
    _pass "budget.warning event appears in failures list"
else
    _fail "budget.warning not in failures (got: ${summary4:0:300})"
fi
echo ""

# Test 12: --host flag accepted (server binds to specified interface)
echo "Test 12: Server accepts --host flag"
python3 src/dashboard/server.py --help 2>&1 | grep -q "\-\-host"
if [[ $? -eq 0 ]]; then
    _pass "--host flag is documented in server --help"
else
    _fail "--host flag not found in server --help"
fi
echo ""

# Test 13: GET /api/throughput returns history array
echo "Test 13: GET /api/throughput returns 30-minute history"
throughput=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/throughput" 2>/dev/null || echo "")
if echo "$throughput" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert 'history' in d, 'missing history field'
assert 'window_minutes' in d, 'missing window_minutes field'
h = d['history']
assert isinstance(h, list), f'history should be list, got {type(h)}'
assert len(h) == 30, f'expected 30 entries, got {len(h)}'
for entry in h:
    assert 'minute' in entry, 'entry missing minute'
    assert 'count' in entry, 'entry missing count'
    assert 'ts' in entry, 'entry missing ts'
" 2>/dev/null; then
    _pass "Throughput endpoint returns 30-entry history with correct shape"
else
    _fail "Throughput endpoint failed (got: ${throughput:0:200})"
fi
echo ""

# Test 14: bead.completed event updates throughput history
echo "Test 14: bead.completed event increments throughput count"
# Ingest a bead.completed event with current timestamp
NOW_TS=$(python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.000Z'))")
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d "{\"type\":\"bead.completed\",\"ts\":\"$NOW_TS\",\"worker\":\"test-worker\",\"data\":{\"bead_id\":\"nd-sparktest\"}}" \
    &>/dev/null || true
throughput2=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/throughput" 2>/dev/null || echo "")
if echo "$throughput2" | python3 -c "
import sys, json
d = json.load(sys.stdin)
h = d.get('history', [])
total = sum(e['count'] for e in h)
assert total >= 1, f'expected at least 1 completion in history, got {total}'
" 2>/dev/null; then
    _pass "bead.completed event appears in throughput history"
else
    _fail "bead.completed not reflected in throughput history (got: ${throughput2:0:200})"
fi
echo ""

# Test 15: Summary includes strand_last_run field
echo "Test 15: Summary includes strand_last_run field"
# Ingest a strand-type event
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d "{\"type\":\"pluck.started\",\"ts\":\"$NOW_TS\",\"worker\":\"test-worker\",\"data\":{}}" \
    &>/dev/null || true
summary5=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary5" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert 'strand_last_run' in d, 'missing strand_last_run in summary'
" 2>/dev/null; then
    _pass "Summary includes strand_last_run field"
else
    _fail "Summary missing strand_last_run (got: ${summary5:0:200})"
fi
echo ""

# Test 16: effort.recorded event populates /api/costs per-bead drill-down
echo "Test 16: effort.recorded event populates /api/costs per-bead drill-down"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"effort.recorded","ts":"2026-03-15T12:00:10.000Z","worker":"pluck-worker","event":"effort.recorded","data":{"bead_id":"nd-cost-drill","cost":"0.042","input_tokens":14000,"output_tokens":7000,"agent":"pluck-worker","strand":"pluck","type":"task"}}' \
    &>/dev/null || true
sleep 0.2
costs_result=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/costs" 2>/dev/null || echo "")
if echo "$costs_result" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert 'by_bead' in d, f'missing by_bead key: {list(d.keys())}'
assert 'total_cost_usd' in d, 'missing total_cost_usd'
assert 'bead_count' in d, 'missing bead_count'
beads = {b['bead_id']: b for b in d['by_bead']}
assert 'nd-cost-drill' in beads, f'nd-cost-drill not found in {list(beads.keys())}'
b = beads['nd-cost-drill']
assert b['input_tokens'] == 14000, f'expected 14000 input_tokens, got {b[\"input_tokens\"]}'
assert b['output_tokens'] == 7000, f'expected 7000 output_tokens, got {b[\"output_tokens\"]}'
assert b['strand'] == 'pluck', f'expected strand=pluck, got {b[\"strand\"]}'
assert b['type'] == 'task', f'expected type=task, got {b[\"type\"]}'
assert b['attempts'] == 1, f'expected 1 attempt, got {b[\"attempts\"]}'
" 2>/dev/null; then
    _pass "/api/costs returns per-bead cost breakdown with correct fields"
else
    _fail "/api/costs missing expected per-bead data (got: ${costs_result:0:400})"
fi
echo ""

# Test 17: /api/summary includes bead_costs field
echo "Test 17: /api/summary includes bead_costs field"
summary6=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary6" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert 'bead_costs' in d, f'missing bead_costs in summary: {list(d.keys())}'
bc = d['bead_costs']
assert 'by_bead' in bc, 'bead_costs missing by_bead'
assert 'total_cost_usd' in bc, 'bead_costs missing total_cost_usd'
assert 'bead_count' in bc, 'bead_costs missing bead_count'
" 2>/dev/null; then
    _pass "Summary includes bead_costs with by_bead drill-down"
else
    _fail "Summary missing bead_costs field (got: ${summary6:0:300})"
fi
echo ""

# Test 18: /api/costs aggregates multiple effort events for same bead
echo "Test 18: /api/costs aggregates multiple effort events for same bead"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"effort.recorded","ts":"2026-03-15T12:00:11.000Z","worker":"pluck-worker","event":"effort.recorded","data":{"bead_id":"nd-cost-drill","cost":"0.008","input_tokens":2000,"output_tokens":1000,"agent":"pluck-worker","strand":"pluck","type":"task"}}' \
    &>/dev/null || true
sleep 0.2
costs2=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/costs" 2>/dev/null || echo "")
if echo "$costs2" | python3 -c "
import sys, json
d = json.load(sys.stdin)
beads = {b['bead_id']: b for b in d['by_bead']}
b = beads.get('nd-cost-drill', {})
assert b.get('attempts', 0) == 2, f'expected 2 attempts, got {b.get(\"attempts\")}'
assert b.get('input_tokens', 0) == 16000, f'expected 16000 input_tokens, got {b.get(\"input_tokens\")}'
" 2>/dev/null; then
    _pass "/api/costs aggregates multiple effort events for same bead"
else
    _fail "/api/costs failed to aggregate multiple effort events (got: ${costs2:0:300})"
fi
echo ""

# Test 19: /api/costs by_bead is sorted by cost descending
echo "Test 19: /api/costs by_bead sorted by cost descending"
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d '{"type":"effort.recorded","ts":"2026-03-15T12:00:12.000Z","worker":"weave-worker","event":"effort.recorded","data":{"bead_id":"nd-cheap-bead","cost":"0.001","input_tokens":100,"output_tokens":50,"agent":"weave-worker","strand":"weave","type":"feature"}}' \
    &>/dev/null || true
sleep 0.2
costs3=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/costs" 2>/dev/null || echo "")
if echo "$costs3" | python3 -c "
import sys, json
d = json.load(sys.stdin)
beads = d.get('by_bead', [])
assert len(beads) >= 2, f'expected at least 2 beads, got {len(beads)}'
costs_list = [b['cost'] for b in beads]
assert costs_list == sorted(costs_list, reverse=True), f'by_bead not sorted desc: {costs_list}'
" 2>/dev/null; then
    _pass "/api/costs by_bead is sorted by cost descending"
else
    _fail "/api/costs by_bead not sorted correctly (got: ${costs3:0:300})"
fi
echo ""

# Test 20: result event with top-level cost_usd (real stream-json format) is tracked
echo "Test 20: result event with cost_usd top-level field is tracked in worker cost"
# Real Claude Code stream-json emits cost_usd at the top level, not inside a data wrapper
curl -sf --max-time 5 -X POST "http://localhost:$TEST_PORT/ingest" \
    -H "Content-Type: application/json" \
    -d "{\"type\":\"result\",\"ts\":\"$NOW_TS\",\"worker\":\"stream-worker\",\"cost_usd\":0.0777,\"usage\":{\"input_tokens\":500,\"output_tokens\":200},\"duration_ms\":5000}" \
    &>/dev/null || true
sleep 0.2
summary_cost=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/api/summary" 2>/dev/null || echo "")
if echo "$summary_cost" | python3 -c "
import sys, json
d = json.load(sys.stdin)
workers = d.get('workers_all', {})
sw = workers.get('stream-worker', {})
assert sw.get('cost', 0) > 0, f'expected cost > 0 for stream-worker, got {sw}'
assert sw.get('tokens_in', 0) == 500, f'expected tokens_in=500, got {sw.get(\"tokens_in\")}'
assert sw.get('tokens_out', 0) == 200, f'expected tokens_out=200, got {sw.get(\"tokens_out\")}'
" 2>/dev/null; then
    _pass "result event with top-level cost_usd is tracked in worker cost"
else
    _fail "result event with cost_usd not tracked (got: ${summary_cost:0:400})"
fi
echo ""

# Test 24: Dashboard HTML contains cost-breakdown panel element
echo "Test 24: Dashboard HTML includes cost breakdown panel"
html_resp=$(curl -sf --max-time 5 "http://localhost:$TEST_PORT/" 2>/dev/null || echo "")
if echo "$html_resp" | grep -q "cost-breakdown-list"; then
    _pass "Dashboard HTML includes cost-breakdown-list element"
else
    _fail "Dashboard HTML missing cost-breakdown-list element"
fi
if echo "$html_resp" | grep -q "Cost Breakdown"; then
    _pass "Dashboard HTML includes 'Cost Breakdown' panel heading"
else
    _fail "Dashboard HTML missing 'Cost Breakdown' panel heading"
fi
echo ""

# ---------------------------------------------------------------------------
# Seed-file tests — start a fresh server instance with --seed-file
# ---------------------------------------------------------------------------

_stop_server

SEED_PORT=17843
SEED_PID=""

_start_seed_server() {
    python3 "$SERVER_SCRIPT" --port "$SEED_PORT" --seed-file "$1" 2>/dev/null &
    SEED_PID=$!
    local retries=20
    while [[ $retries -gt 0 ]]; do
        if curl -sf --max-time 1 "http://localhost:$SEED_PORT/health" &>/dev/null; then
            return 0
        fi
        sleep 0.2
        retries=$((retries - 1))
    done
    echo "ERROR: Seed server did not start in time" >&2
    return 1
}

_stop_seed_server() {
    if [[ -n "$SEED_PID" ]] && kill -0 "$SEED_PID" 2>/dev/null; then
        kill "$SEED_PID" 2>/dev/null
        wait "$SEED_PID" 2>/dev/null || true
        SEED_PID=""
    fi
}

# Build a temp seed file
SEED_FILE="$(mktemp /tmp/fabric-seed-XXXXXX.jsonl)"
trap '_stop_seed_server; _stop_server; rm -f "$SEED_FILE"' EXIT

SEED_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
cat >"$SEED_FILE" <<EOF
{"type":"bead.completed","ts":"$SEED_TS","worker":"seeded-worker","bead_id":"nd-test1"}
{"type":"bead.completed","ts":"$SEED_TS","worker":"seeded-worker","bead_id":"nd-test2"}
{"type":"bead.failed","ts":"$SEED_TS","worker":"seeded-worker","data":{"bead_id":"nd-testF","reason":"timeout"}}
EOF

if ! _start_seed_server "$SEED_FILE"; then
    echo "SKIP: Could not start seed server"
else

# Test 21: --seed-file loads events into the buffer
echo "Test 21: --seed-file populates event buffer"
seed_events=$(curl -sf --max-time 5 "http://localhost:$SEED_PORT/api/events" 2>/dev/null || echo "")
if echo "$seed_events" | python3 -c "
import sys, json
d = json.load(sys.stdin)
assert d.get('count', 0) >= 3, f'expected >= 3 seeded events, got count={d.get(\"count\")}'
" 2>/dev/null; then
    _pass "--seed-file populates event buffer with seeded events"
else
    _fail "--seed-file did not populate event buffer (got: ${seed_events:0:200})"
fi
echo ""

# Test 22: seeded bead.completed events appear in throughput history
echo "Test 22: --seed-file updates throughput sparkline for bead.completed events"
seed_throughput=$(curl -sf --max-time 5 "http://localhost:$SEED_PORT/api/throughput" 2>/dev/null || echo "")
if echo "$seed_throughput" | python3 -c "
import sys, json
d = json.load(sys.stdin)
history = d.get('history', [])
total = sum(h['count'] for h in history)
assert total >= 2, f'expected >= 2 seeded completions in throughput, got total={total}'
" 2>/dev/null; then
    _pass "seeded bead.completed events update throughput sparkline"
else
    _fail "seeded bead.completed events not reflected in throughput (got: ${seed_throughput:0:200})"
fi
echo ""

# Test 23: seeded bead.failed event appears in summary failures
echo "Test 23: --seed-file populates failure alerts from bead.failed events"
seed_summary=$(curl -sf --max-time 5 "http://localhost:$SEED_PORT/api/summary" 2>/dev/null || echo "")
if echo "$seed_summary" | python3 -c "
import sys, json
d = json.load(sys.stdin)
failures = d.get('failures', [])
assert any(f.get('type') == 'bead_failure' for f in failures), f'expected bead_failure in failures, got {failures}'
" 2>/dev/null; then
    _pass "seeded bead.failed event appears in summary failures"
else
    _fail "seeded bead.failed event not in summary failures (got: ${seed_summary:0:400})"
fi
echo ""

fi  # _start_seed_server

_stop_seed_server

# Summary
echo "=== Results: $pass passed, $fail failed ==="
if [[ $fail -gt 0 ]]; then
    exit 1
fi
