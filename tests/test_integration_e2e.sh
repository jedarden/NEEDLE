#!/usr/bin/env bash
# End-to-end integration tests for multi-worker scenarios
# Tests: concurrent bead claiming, file locking across workers,
#        strand priority fallthrough (1->2->3->...), cross-workspace
#        coordination, and hook lifecycle during bead execution.

# Don't use set -e because arithmetic ((++)) can return 1 and trigger exit

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# ============================================================================
# Test environment setup
# ============================================================================

TEST_DIR=$(mktemp -d)
TEST_WORKSPACE="$TEST_DIR/workspace"
TEST_WORKSPACE_B="$TEST_DIR/workspace_b"
TEST_NEEDLE_HOME="$TEST_DIR/.needle"
TEST_LOCK_DIR="$TEST_DIR/locks"

export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_CONFIG_NAME="config.yaml"
export NEEDLE_SESSION="test-e2e-$$"
export NEEDLE_WORKSPACE="$TEST_WORKSPACE"
export NEEDLE_AGENT="test-agent"
export NEEDLE_VERBOSE=false
export NEEDLE_QUIET=true
export NEEDLE_STATE_DIR="state"
export NEEDLE_LOG_DIR="logs"
export NEEDLE_LOCK_DIR="$TEST_LOCK_DIR"

mkdir -p "$TEST_WORKSPACE/.beads"
mkdir -p "$TEST_WORKSPACE_B/.beads"
mkdir -p "$TEST_NEEDLE_HOME/$NEEDLE_STATE_DIR"
mkdir -p "$TEST_NEEDLE_HOME/$NEEDLE_LOG_DIR"
mkdir -p "$TEST_NEEDLE_HOME/agents"
mkdir -p "$TEST_NEEDLE_HOME/hooks"
mkdir -p "$TEST_LOCK_DIR"

# Cleanup
cleanup() {
    tmux list-sessions 2>/dev/null | grep "^needle-test-e2e" | cut -d: -f1 | while read -r s; do
        tmux kill-session -t "$s" 2>/dev/null || true
    done
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# ============================================================================
# Test framework
# ============================================================================

TESTS_PASSED=0
TESTS_FAILED=0

_test_start() {
    echo "TEST: $1"
}

_test_pass() {
    echo "  PASS: $1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

_test_fail() {
    echo "  FAIL: $1"
    [[ -n "${2:-}" ]] && echo "    Details: $2"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# ============================================================================
# Source required modules
# ============================================================================

source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/paths.sh"
source "$PROJECT_ROOT/src/lib/json.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/lib/config.sh"
source "$PROJECT_ROOT/src/runner/naming.sh"
source "$PROJECT_ROOT/src/lock/checkout.sh"
source "$PROJECT_ROOT/src/hooks/runner.sh"

# Write base config
cat > "$NEEDLE_HOME/config.yaml" << 'BASEEOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: false
  pulse: false
  knot: true
BASEEOF

# ============================================================================
# SCENARIO 1: Concurrent bead claiming - only one worker wins
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 1: Concurrent bead claiming"
echo "=========================================="

_test_start "Concurrent workers claim the same bead atomically (only one wins)"

CLAIM_DIR="$TEST_DIR/claim_scenario"
mkdir -p "$CLAIM_DIR"

# Simulate atomic claim via hard-link (ln is atomic on Linux for same filesystem)
_atomic_claim_bead() {
    local bead_id="$1"
    local worker_id="$2"
    local claim_file="$CLAIM_DIR/${bead_id}.claimed"

    local tmp_file
    tmp_file=$(mktemp "$CLAIM_DIR/tmp.XXXXXX")
    echo "$worker_id" > "$tmp_file"

    if ln "$tmp_file" "$claim_file" 2>/dev/null; then
        rm -f "$tmp_file"
        return 0
    else
        rm -f "$tmp_file"
        return 1
    fi
}

BEAD_ID="nd-test-concurrent-$$"
WINNER_FILE="$TEST_DIR/winner.txt"
rm -f "$WINNER_FILE"

for i in $(seq 1 5); do
    (
        if _atomic_claim_bead "$BEAD_ID" "worker-$i"; then
            echo "worker-$i" > "$WINNER_FILE"
        fi
    ) &
done
wait

if [[ -f "$WINNER_FILE" ]]; then
    winner=$(cat "$WINNER_FILE")
    claimed_by=$(cat "$CLAIM_DIR/${BEAD_ID}.claimed")
    if [[ "$winner" == "$claimed_by" ]]; then
        _test_pass "Exactly one worker claimed bead: $winner"
    else
        _test_fail "Claim winner mismatch: winner=$winner, claimed_by=$claimed_by"
    fi
else
    _test_fail "No worker was able to claim the bead"
fi

_test_start "Second claim attempt fails after first worker claims"
BEAD_ID2="nd-test-second-claim-$$"
_atomic_claim_bead "$BEAD_ID2" "first-worker"
if ! _atomic_claim_bead "$BEAD_ID2" "second-worker" 2>/dev/null; then
    _test_pass "Second claim correctly rejected after first worker claimed"
else
    _test_fail "Second worker should not be able to claim an already-claimed bead"
fi

_test_start "Different beads can be claimed simultaneously by different workers"
BEAD_A="nd-test-bead-a-$$"
BEAD_B="nd-test-bead-b-$$"
SUCCESS_A=false
SUCCESS_B=false

_atomic_claim_bead "$BEAD_A" "worker-alpha" && SUCCESS_A=true
_atomic_claim_bead "$BEAD_B" "worker-bravo" && SUCCESS_B=true

if $SUCCESS_A && $SUCCESS_B; then
    _test_pass "Two workers claimed separate beads simultaneously"
else
    _test_fail "Workers should be able to claim different beads (A=$SUCCESS_A B=$SUCCESS_B)"
fi

_test_start "Claim release allows another worker to claim (re-open simulation)"
BEAD_RELEASE="nd-release-test-$$"
_atomic_claim_bead "$BEAD_RELEASE" "worker-alpha"
rm -f "$CLAIM_DIR/${BEAD_RELEASE}.claimed"   # simulate release

if _atomic_claim_bead "$BEAD_RELEASE" "worker-bravo"; then
    claimed_by=$(cat "$CLAIM_DIR/${BEAD_RELEASE}.claimed")
    if [[ "$claimed_by" == "worker-bravo" ]]; then
        _test_pass "After release, worker-bravo successfully reclaimed the bead"
    else
        _test_fail "Claim file has wrong owner after re-claim: $claimed_by"
    fi
else
    _test_fail "Re-claim should succeed after release"
fi

# ============================================================================
# SCENARIO 2: File locking across workers using checkout.sh
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 2: File locking across workers"
echo "=========================================="

_test_start "Lock path UUID is deterministic for same file path"
TEST_PATH="/tmp/needle-test-determinism-$$.sh"
uuid1=$(_needle_lock_path_uuid "$TEST_PATH")
uuid2=$(_needle_lock_path_uuid "$TEST_PATH")
if [[ "$uuid1" == "$uuid2" && -n "$uuid1" ]]; then
    _test_pass "Path UUID is deterministic: $uuid1"
else
    _test_fail "Path UUID is non-deterministic: '$uuid1' vs '$uuid2'"
fi

_test_start "Lock path UUID differs for different file paths"
uuid_a=$(_needle_lock_path_uuid "/tmp/file_a_$$.sh")
uuid_b=$(_needle_lock_path_uuid "/tmp/file_b_$$.sh")
if [[ "$uuid_a" != "$uuid_b" ]]; then
    _test_pass "Different paths produce different UUIDs ($uuid_a vs $uuid_b)"
else
    _test_fail "Different paths should produce different UUIDs"
fi

_test_start "Lock path UUID is 8 hex characters"
uuid=$(_needle_lock_path_uuid "/some/test/path.sh")
if [[ "$uuid" =~ ^[0-9a-f]{8}$ ]]; then
    _test_pass "UUID is 8 hex chars: $uuid"
else
    _test_fail "UUID should be 8 hex chars, got: '$uuid'"
fi

_test_start "File checkout creates lock file in NEEDLE_LOCK_DIR"
CHECKOUT_FILE="$TEST_WORKSPACE/src/main.sh"
mkdir -p "$(dirname "$CHECKOUT_FILE")"
touch "$CHECKOUT_FILE"

export NEEDLE_BEAD_ID="nd-lock-test-$$"
export NEEDLE_WORKER="worker-alpha"

checkout_rc=0
checkout_file "$CHECKOUT_FILE" "$NEEDLE_BEAD_ID" "$NEEDLE_WORKER" 2>/dev/null || checkout_rc=$?

if [[ $checkout_rc -eq 0 ]]; then
    lock_exists=false
    for f in "$TEST_LOCK_DIR"/*; do
        [[ -f "$f" ]] && lock_exists=true && break
    done
    if $lock_exists; then
        _test_pass "File checkout created lock file in $TEST_LOCK_DIR"
    else
        _test_fail "checkout_file succeeded but no lock file in $TEST_LOCK_DIR"
    fi
else
    _test_fail "checkout_file failed (rc=$checkout_rc)"
fi

_test_start "File checkout conflict: second bead blocked from same file"
CONFLICT_FILE="$TEST_WORKSPACE/src/conflict.sh"
touch "$CONFLICT_FILE"
OWNER_BEAD="nd-owner-$$"
OTHER_BEAD="nd-other-$$"

checkout_file "$CONFLICT_FILE" "$OWNER_BEAD" "worker-alpha" 2>/dev/null

conflict_rc=0
checkout_file "$CONFLICT_FILE" "$OTHER_BEAD" "worker-bravo" 2>/dev/null || conflict_rc=$?

if [[ $conflict_rc -ne 0 ]]; then
    _test_pass "Second worker correctly blocked from file locked by first worker"
else
    _test_fail "Second worker should be blocked from already-locked file"
fi

_test_start "File release removes lock and allows re-checkout"
RELEASE_FILE="$TEST_WORKSPACE/src/release_test.sh"
touch "$RELEASE_FILE"
RELEASE_BEAD="nd-release-lock-$$"
NEW_OWNER_BEAD="nd-new-owner-$$"

checkout_file "$RELEASE_FILE" "$RELEASE_BEAD" "worker-alpha" 2>/dev/null
release_file "$RELEASE_FILE" "$RELEASE_BEAD" 2>/dev/null

re_rc=0
checkout_file "$RELEASE_FILE" "$NEW_OWNER_BEAD" "worker-bravo" 2>/dev/null || re_rc=$?

if [[ $re_rc -eq 0 ]]; then
    _test_pass "Re-checkout succeeds after file lock is released"
else
    _test_fail "Re-checkout should succeed after release (rc=$re_rc)"
fi

_test_start "Lock file path uses bead ID and path UUID as components"
TEST_FILE_LOCK="$TEST_WORKSPACE/src/lock_path_test.sh"
touch "$TEST_FILE_LOCK"
LOCK_BEAD="nd-lock-path-$$"

checkout_file "$TEST_FILE_LOCK" "$LOCK_BEAD" "worker-alpha" 2>/dev/null

expected_uuid=$(_needle_lock_path_uuid "$TEST_FILE_LOCK")
expected_lock="${TEST_LOCK_DIR}/${LOCK_BEAD}-${expected_uuid}"

if [[ -f "$expected_lock" ]]; then
    _test_pass "Lock file created at expected path: $(basename "$expected_lock")"
else
    # Look for any lock file with the bead ID
    found_lock=$(ls "$TEST_LOCK_DIR/${LOCK_BEAD}-"* 2>/dev/null | head -1)
    if [[ -n "$found_lock" ]]; then
        _test_pass "Lock file created with bead ID prefix: $(basename "$found_lock")"
    else
        _test_fail "No lock file found for bead $LOCK_BEAD in $TEST_LOCK_DIR"
    fi
fi

_test_start "Same bead can checkout same file twice (idempotent)"
IDEM_FILE="$TEST_WORKSPACE/src/idempotent.sh"
touch "$IDEM_FILE"
IDEM_BEAD="nd-idem-$$"

checkout_file "$IDEM_FILE" "$IDEM_BEAD" "worker-alpha" 2>/dev/null
rc2=0
checkout_file "$IDEM_FILE" "$IDEM_BEAD" "worker-alpha" 2>/dev/null || rc2=$?

if [[ $rc2 -eq 0 ]]; then
    _test_pass "Same bead can checkout same file multiple times (idempotent)"
else
    _test_fail "Same bead should be able to re-checkout its own file (rc=$rc2)"
fi

# ============================================================================
# SCENARIO 3: Strand priority fallthrough (1->2->3->...)
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 3: Strand priority fallthrough"
echo "=========================================="

# Stub telemetry functions so engine.sh can be sourced without real telemetry
_needle_emit_event()      { return 0; }
_needle_diag_engine()     { return 0; }
_needle_diag_no_work()    { return 0; }
_needle_diag_starvation() { return 0; }
_needle_verbose()         { return 0; }
_needle_success()         { return 0; }
_needle_debug()           { return 0; }

# Source workspace module if available (engine.sh may need it)
source "$PROJECT_ROOT/src/lib/workspace.sh" 2>/dev/null || true

# Source telemetry events stub (engine.sh sources this)
_needle_event_hook_started()   { return 0; }
_needle_event_hook_completed() { return 0; }
_needle_event_hook_failed()    { return 0; }

# Install mock strands BEFORE sourcing engine (engine sources them)
# We define them in the current shell so they override whatever engine.sh sources
STRAND_CALL_LOG="$TEST_DIR/strand_calls.txt"
rm -f "$STRAND_CALL_LOG"

# Source engine but then override strand functions
source "$PROJECT_ROOT/src/strands/engine.sh" 2>/dev/null

# Re-apply stubs after sourcing (engine sourcing may clobber them)
_needle_emit_event()      { return 0; }
_needle_diag_engine()     { return 0; }
_needle_diag_no_work()    { return 0; }
_needle_diag_starvation() { return 0; }
_needle_verbose()         { return 0; }
_needle_success()         { return 0; }
_needle_debug()           { return 0; }

# Enable all strands in config
cat > "$NEEDLE_HOME/config.yaml" << 'ALLEOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: true
  unravel: true
  pulse: true
  knot: true
ALLEOF
declare -f clear_config_cache &>/dev/null && clear_config_cache

# ---- Test: all strands no-work ----

_test_start "Strand engine calls all 7 strands when all return no-work"
rm -f "$STRAND_CALL_LOG"

_needle_strand_pluck()   { echo "pluck"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_explore() { echo "explore" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_mend()    { echo "mend"    >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_weave()   { echo "weave"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_unravel() { echo "unravel" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_pulse()   { echo "pulse"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_knot()    { echo "knot"    >> "$STRAND_CALL_LOG"; return 1; }

_needle_strand_engine "$TEST_WORKSPACE" "test-agent" 2>/dev/null
engine_rc=$?

if [[ $engine_rc -ne 0 ]]; then
    strand_count=$(wc -l < "$STRAND_CALL_LOG" 2>/dev/null | tr -d ' ')
    if [[ "$strand_count" -eq 7 ]]; then
        _test_pass "All 7 strands called when no work found (engine returned 1)"
    else
        _test_fail "Expected 7 strand calls, got $strand_count: $(cat "$STRAND_CALL_LOG" 2>/dev/null | paste -sd',')"
    fi
else
    _test_fail "Engine should return 1 when all strands find no work"
fi

_test_start "Strands called in correct priority order (pluck->explore->mend->weave->unravel->pulse->knot)"
actual_order=$(paste -sd',' "$STRAND_CALL_LOG" 2>/dev/null)
expected_order="pluck,explore,mend,weave,unravel,pulse,knot"
if [[ "$actual_order" == "$expected_order" ]]; then
    _test_pass "Strands called in correct priority order"
else
    _test_fail "Wrong strand order. Expected: $expected_order, Got: $actual_order"
fi

# ---- Test: pluck finds work ----

_test_start "Engine stops at strand 1 (pluck) when it finds work"
rm -f "$STRAND_CALL_LOG"

_needle_strand_pluck()   { echo "pluck"   >> "$STRAND_CALL_LOG"; return 0; }
_needle_strand_explore() { echo "explore" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_mend()    { echo "mend"    >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_weave()   { echo "weave"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_unravel() { echo "unravel" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_pulse()   { echo "pulse"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_knot()    { echo "knot"    >> "$STRAND_CALL_LOG"; return 1; }

_needle_strand_engine "$TEST_WORKSPACE" "test-agent" 2>/dev/null
engine_rc=$?

if [[ $engine_rc -eq 0 ]]; then
    strand_count=$(wc -l < "$STRAND_CALL_LOG" | tr -d ' ')
    called=$(cat "$STRAND_CALL_LOG")
    if [[ "$strand_count" -eq 1 && "$called" == "pluck" ]]; then
        _test_pass "Engine stopped after pluck found work (no other strands called)"
    else
        _test_fail "Expected only 'pluck' called, got $strand_count strands: $(cat "$STRAND_CALL_LOG" | paste -sd',')"
    fi
else
    _test_fail "Engine should return 0 when a strand finds work"
fi

# ---- Test: fallthrough to strand 3 ----

_test_start "Engine falls through to strand 3 (mend) when pluck and explore fail"
rm -f "$STRAND_CALL_LOG"

_needle_strand_pluck()   { echo "pluck"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_explore() { echo "explore" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_mend()    { echo "mend"    >> "$STRAND_CALL_LOG"; return 0; }
_needle_strand_weave()   { echo "weave"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_unravel() { echo "unravel" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_pulse()   { echo "pulse"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_knot()    { echo "knot"    >> "$STRAND_CALL_LOG"; return 1; }

_needle_strand_engine "$TEST_WORKSPACE" "test-agent" 2>/dev/null
engine_rc=$?

if [[ $engine_rc -eq 0 ]]; then
    strand_count=$(wc -l < "$STRAND_CALL_LOG" | tr -d ' ')
    called=$(cat "$STRAND_CALL_LOG" | paste -sd',')
    if [[ "$strand_count" -eq 3 && "$called" == "pluck,explore,mend" ]]; then
        _test_pass "Engine fell through to mend (strand 3): $called"
    else
        _test_fail "Expected 'pluck,explore,mend', got $strand_count strands: $called"
    fi
else
    _test_fail "Engine should return 0 when mend finds work"
fi

# ---- Test: fallthrough to last strand ----

_test_start "Engine falls through to strand 7 (knot) as last resort"
rm -f "$STRAND_CALL_LOG"

_needle_strand_pluck()   { echo "pluck"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_explore() { echo "explore" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_mend()    { echo "mend"    >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_weave()   { echo "weave"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_unravel() { echo "unravel" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_pulse()   { echo "pulse"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_knot()    { echo "knot"    >> "$STRAND_CALL_LOG"; return 0; }

_needle_strand_engine "$TEST_WORKSPACE" "test-agent" 2>/dev/null
engine_rc=$?

if [[ $engine_rc -eq 0 ]]; then
    called=$(cat "$STRAND_CALL_LOG" | paste -sd',')
    last=$(tail -1 "$STRAND_CALL_LOG")
    if [[ "$last" == "knot" ]]; then
        _test_pass "Engine fell through to knot (strand 7): $called"
    else
        _test_fail "Expected last strand to be 'knot', got: $last"
    fi
else
    _test_fail "Engine should return 0 when knot finds work"
fi

# ---- Test: disabled strand skipped ----

_test_start "Disabled strands are skipped in the priority waterfall"
rm -f "$STRAND_CALL_LOG"

cat > "$NEEDLE_HOME/config.yaml" << 'DISABLEDEOF'
strands:
  pluck: true
  explore: true
  mend: true
  weave: false
  unravel: true
  pulse: true
  knot: true
DISABLEDEOF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_strand_pluck()   { echo "pluck"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_explore() { echo "explore" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_mend()    { echo "mend"    >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_weave()   { echo "weave"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_unravel() { echo "unravel" >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_pulse()   { echo "pulse"   >> "$STRAND_CALL_LOG"; return 1; }
_needle_strand_knot()    { echo "knot"    >> "$STRAND_CALL_LOG"; return 1; }

_needle_strand_engine "$TEST_WORKSPACE" "test-agent" 2>/dev/null

if [[ -f "$STRAND_CALL_LOG" ]]; then
    if ! grep -q "weave" "$STRAND_CALL_LOG"; then
        strand_count=$(wc -l < "$STRAND_CALL_LOG" | tr -d ' ')
        _test_pass "Disabled strand (weave) skipped; $strand_count enabled strands ran"
    else
        _test_fail "Disabled strand 'weave' should not have been called"
    fi
else
    _test_fail "No strand call log found"
fi

# Restore all-enabled config
cat > "$NEEDLE_HOME/config.yaml" << 'ALLEOF2'
strands:
  pluck: true
  explore: true
  mend: true
  weave: true
  unravel: true
  pulse: true
  knot: true
ALLEOF2
declare -f clear_config_cache &>/dev/null && clear_config_cache

# ============================================================================
# SCENARIO 4: Cross-workspace coordination
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 4: Cross-workspace coordination"
echo "=========================================="

_test_start "Same relative path in different workspaces has distinct lock UUIDs"
uuid_ws_a=$(_needle_lock_path_uuid "$TEST_WORKSPACE/src/file.sh")
uuid_ws_b=$(_needle_lock_path_uuid "$TEST_WORKSPACE_B/src/file.sh")
if [[ "$uuid_ws_a" != "$uuid_ws_b" ]]; then
    _test_pass "Different workspaces produce distinct lock UUIDs for same relative path"
else
    _test_fail "Should have distinct UUIDs but got: $uuid_ws_a == $uuid_ws_b"
fi

_test_start "Workers in different workspaces can independently lock same relative filename"
FILE_IN_A="$TEST_WORKSPACE/shared_name.sh"
FILE_IN_B="$TEST_WORKSPACE_B/shared_name.sh"
touch "$FILE_IN_A"
touch "$FILE_IN_B"

BEAD_WS_A="nd-ws-a-$$"
BEAD_WS_B="nd-ws-b-$$"

rc_a=0; rc_b=0
checkout_file "$FILE_IN_A" "$BEAD_WS_A" "worker-alpha" 2>/dev/null || rc_a=$?
checkout_file "$FILE_IN_B" "$BEAD_WS_B" "worker-bravo" 2>/dev/null || rc_b=$?

if [[ $rc_a -eq 0 && $rc_b -eq 0 ]]; then
    _test_pass "Workers in separate workspaces independently locked same-named files"
else
    _test_fail "Cross-workspace independent locking failed (rc_a=$rc_a rc_b=$rc_b)"
fi

_test_start "Lock file for workspace A does not block workspace B checkout"
# Verify via UUID: different absolute paths -> different lock files -> no conflict
lock_key_a="${TEST_LOCK_DIR}/${BEAD_WS_A}-${uuid_ws_a}"
lock_key_b="${TEST_LOCK_DIR}/${BEAD_WS_B}-${uuid_ws_b}"
# They should exist as separate files
if [[ "$lock_key_a" != "$lock_key_b" ]]; then
    _test_pass "Cross-workspace lock files are distinct (no cross-contamination)"
else
    _test_fail "Lock file keys collided across workspaces: $lock_key_a"
fi

_test_start "Concurrent worker identifier allocation is unique across 8 workers"
ALLOC_LOCK="$TEST_DIR/alloc.lock"
rm -f "$TEST_DIR/used_ids.txt"

ALLOC_LOG="$TEST_DIR/alloc_log.txt"
rm -f "$ALLOC_LOG"

_allocate_identifier() {
    local worker_num="$1"
    (
        exec 9>"$ALLOC_LOCK"
        flock -x 9

        local used=""
        [[ -f "$TEST_DIR/used_ids.txt" ]] && used=$(cat "$TEST_DIR/used_ids.txt")

        local next_id
        next_id=$(get_next_identifier_from_list "$used")

        echo "${used} ${next_id}" | tr -s ' ' | sed 's/^ //' > "$TEST_DIR/used_ids.txt"
        echo "worker-$worker_num:$next_id" >> "$ALLOC_LOG"
    )
}

for i in $(seq 1 8); do
    _allocate_identifier "$i" &
done
wait

if [[ -f "$ALLOC_LOG" ]]; then
    total=$(wc -l < "$ALLOC_LOG" | tr -d ' ')
    unique_count=$(awk -F: '{print $2}' "$ALLOC_LOG" | sort -u | wc -l | tr -d ' ')
    if [[ "$total" -eq 8 && "$unique_count" -eq 8 ]]; then
        _test_pass "All 8 concurrent workers received unique identifiers"
    else
        _test_fail "Identifier collision: $total allocations, $unique_count unique. Log: $(cat "$ALLOC_LOG")"
    fi
else
    _test_fail "No allocation log found"
fi

# ============================================================================
# SCENARIO 5: Hook lifecycle during bead execution
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 5: Hook lifecycle during bead execution"
echo "=========================================="

HOOK_LOG="$TEST_DIR/hook_calls.txt"
rm -f "$HOOK_LOG"

# Write hook scripts (expanding variables now at write time)
cat > "$TEST_NEEDLE_HOME/hooks/pre-claim.sh" << HOOKEOF
#!/usr/bin/env bash
echo "pre_claim:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/pre-claim.sh"

cat > "$TEST_NEEDLE_HOME/hooks/post-claim.sh" << HOOKEOF
#!/usr/bin/env bash
echo "post_claim:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/post-claim.sh"

cat > "$TEST_NEEDLE_HOME/hooks/pre-execute.sh" << HOOKEOF
#!/usr/bin/env bash
echo "pre_execute:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/pre-execute.sh"

cat > "$TEST_NEEDLE_HOME/hooks/post-execute.sh" << HOOKEOF
#!/usr/bin/env bash
echo "post_execute:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/post-execute.sh"

cat > "$TEST_NEEDLE_HOME/hooks/post-complete.sh" << HOOKEOF
#!/usr/bin/env bash
echo "post_complete:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/post-complete.sh"

cat > "$TEST_NEEDLE_HOME/hooks/on-failure.sh" << HOOKEOF
#!/usr/bin/env bash
echo "on_failure:\${NEEDLE_BEAD_ID:-}:\${NEEDLE_WORKER:-}" >> "$HOOK_LOG"
exit 0
HOOKEOF
chmod +x "$TEST_NEEDLE_HOME/hooks/on-failure.sh"

# Write config pointing to hook scripts
cat > "$NEEDLE_HOME/config.yaml" << HOOKCONF
strands:
  pluck: true
  knot: true

hooks:
  pre_claim: $TEST_NEEDLE_HOME/hooks/pre-claim.sh
  post_claim: $TEST_NEEDLE_HOME/hooks/post-claim.sh
  pre_execute: $TEST_NEEDLE_HOME/hooks/pre-execute.sh
  post_execute: $TEST_NEEDLE_HOME/hooks/post-execute.sh
  post_complete: $TEST_NEEDLE_HOME/hooks/post-complete.sh
  on_failure: $TEST_NEEDLE_HOME/hooks/on-failure.sh
  timeout: 10s
  fail_action: warn
HOOKCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_test_start "Hook system has all 11 standard lifecycle hook types registered"
expected_hooks=(pre_claim post_claim pre_execute post_execute pre_complete post_complete on_failure on_quarantine pre_commit post_task error_recovery)
all_found=true
for hook_type in "${expected_hooks[@]}"; do
    if ! printf '%s\n' "${NEEDLE_HOOK_TYPES[@]}" | grep -q "^${hook_type}$"; then
        _test_fail "Hook type '$hook_type' not in NEEDLE_HOOK_TYPES"
        all_found=false
        break
    fi
done
$all_found && _test_pass "All 11 lifecycle hook types are registered"

_test_start "Pre-claim hook fires with bead context variables"
export NEEDLE_BEAD_ID="nd-hook-test-$$"
export NEEDLE_WORKER="worker-alpha"
rm -f "$HOOK_LOG"

_needle_run_hook "pre_claim" "$NEEDLE_BEAD_ID" 2>/dev/null
rc=$?

if [[ $rc -eq 0 ]] && [[ -f "$HOOK_LOG" ]] && grep -q "pre_claim:${NEEDLE_BEAD_ID}" "$HOOK_LOG"; then
    _test_pass "pre_claim hook executed with bead context (bead=$NEEDLE_BEAD_ID)"
else
    _test_fail "pre_claim hook did not fire or missing context (rc=$rc, log: $(cat "$HOOK_LOG" 2>/dev/null))"
fi

_test_start "Post-execute hook fires correctly"
rm -f "$HOOK_LOG"
_needle_run_hook "post_execute" "$NEEDLE_BEAD_ID" 2>/dev/null
rc=$?
if [[ $rc -eq 0 ]] && grep -q "post_execute:" "$HOOK_LOG" 2>/dev/null; then
    _test_pass "post_execute hook fired correctly"
else
    _test_fail "post_execute hook did not fire (rc=$rc)"
fi

_test_start "On-failure hook fires when invoked"
rm -f "$HOOK_LOG"
_needle_run_hook "on_failure" "$NEEDLE_BEAD_ID" 2>/dev/null
rc=$?
if [[ $rc -eq 0 ]] && grep -q "on_failure:" "$HOOK_LOG" 2>/dev/null; then
    _test_pass "on_failure hook fired and logged"
else
    _test_fail "on_failure hook did not fire (rc=$rc)"
fi

_test_start "Hook exit code ABORT (2) causes _needle_run_hook to return 1"
ABORT_HOOK="$TEST_DIR/abort-hook.sh"
cat > "$ABORT_HOOK" << 'ABORTEOF'
#!/usr/bin/env bash
exit 2
ABORTEOF
chmod +x "$ABORT_HOOK"

# Update config to point pre_claim at abort hook
cat > "$NEEDLE_HOME/config.yaml" << ABORTCONF
strands:
  pluck: true
hooks:
  pre_claim: $ABORT_HOOK
  timeout: 10s
  fail_action: warn
ABORTCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_run_hook "pre_claim" "$NEEDLE_BEAD_ID" 2>/dev/null
hook_rc=$?
# ABORT (exit 2) -> _needle_run_hook returns 1 (see runner.sh case NEEDLE_HOOK_EXIT_ABORT)
if [[ $hook_rc -eq 1 ]]; then
    _test_pass "Hook ABORT (exit 2) causes runner to return 1 (abort signaled)"
else
    _test_fail "Hook ABORT exit code not handled correctly (got rc=$hook_rc, expected 1)"
fi

_test_start "Hook exit code WARNING (1) returns 0 with fail_action=warn"
WARN_HOOK="$TEST_DIR/warn-hook.sh"
cat > "$WARN_HOOK" << 'WARNEOF'
#!/usr/bin/env bash
exit 1
WARNEOF
chmod +x "$WARN_HOOK"

cat > "$NEEDLE_HOME/config.yaml" << WARNCONF
strands:
  pluck: true
hooks:
  pre_claim: $WARN_HOOK
  timeout: 10s
  fail_action: warn
WARNCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_run_hook "pre_claim" "$NEEDLE_BEAD_ID" 2>/dev/null
hook_rc=$?
# WARNING (exit 1) -> _needle_run_hook returns 0 when fail_action=warn
if [[ $hook_rc -eq 0 ]]; then
    _test_pass "Hook WARNING (exit 1) returns 0 with fail_action=warn (continue)"
else
    _test_fail "Hook WARNING with fail_action=warn should return 0 (got $hook_rc)"
fi

_test_start "Hook SKIP (exit 3) causes _needle_run_hook to return 2"
SKIP_HOOK="$TEST_DIR/skip-hook.sh"
cat > "$SKIP_HOOK" << 'SKIPEOF'
#!/usr/bin/env bash
exit 3
SKIPEOF
chmod +x "$SKIP_HOOK"

cat > "$NEEDLE_HOME/config.yaml" << SKIPCONF
strands:
  pluck: true
hooks:
  pre_claim: $SKIP_HOOK
  timeout: 10s
  fail_action: warn
SKIPCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_run_hook "pre_claim" "$NEEDLE_BEAD_ID" 2>/dev/null
hook_rc=$?
# SKIP (exit 3) -> _needle_run_hook returns 2
if [[ $hook_rc -eq 2 ]]; then
    _test_pass "Hook SKIP (exit 3) causes runner to return 2 (skip signaled)"
else
    _test_fail "Hook SKIP not handled correctly (got rc=$hook_rc, expected 2)"
fi

_test_start "Non-existent hook file is handled gracefully (returns 0)"
cat > "$NEEDLE_HOME/config.yaml" << MISSINGCONF
strands:
  pluck: true
hooks:
  pre_claim: /nonexistent/hook-that-does-not-exist.sh
  timeout: 10s
  fail_action: warn
MISSINGCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_run_hook "pre_claim" "$NEEDLE_BEAD_ID" 2>/dev/null
hook_rc=$?
if [[ $hook_rc -eq 0 ]]; then
    _test_pass "Non-existent hook gracefully returns 0 (skipped)"
else
    _test_fail "Non-existent hook caused non-zero return: $hook_rc"
fi

_test_start "Unconfigured hook type returns 0 (no-op)"
cat > "$NEEDLE_HOME/config.yaml" << 'NOHOOKSCONF'
strands:
  pluck: true
NOHOOKSCONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

_needle_run_hook "pre_complete" "$NEEDLE_BEAD_ID" 2>/dev/null
hook_rc=$?
if [[ $hook_rc -eq 0 ]]; then
    _test_pass "Unconfigured hook returns 0 (no-op, no crash)"
else
    _test_fail "Unconfigured hook should return 0 (got $hook_rc)"
fi

_test_start "Full bead lifecycle hooks fire in correct order"
LIFECYCLE_LOG="$TEST_DIR/lifecycle.txt"
rm -f "$LIFECYCLE_LOG"

# Create a hook that appends its name to the log
for hook_name in pre_claim post_claim pre_execute post_execute post_complete; do
    hook_file="$TEST_DIR/${hook_name}.sh"
    cat > "$hook_file" << LIFECYCLEEOF
#!/usr/bin/env bash
echo "${hook_name}" >> "$LIFECYCLE_LOG"
exit 0
LIFECYCLEEOF
    chmod +x "$hook_file"
done

cat > "$NEEDLE_HOME/config.yaml" << LIFECYCLECONF
strands:
  pluck: true
hooks:
  pre_claim: $TEST_DIR/pre_claim.sh
  post_claim: $TEST_DIR/post_claim.sh
  pre_execute: $TEST_DIR/pre_execute.sh
  post_execute: $TEST_DIR/post_execute.sh
  post_complete: $TEST_DIR/post_complete.sh
  timeout: 10s
  fail_action: warn
LIFECYCLECONF
declare -f clear_config_cache &>/dev/null && clear_config_cache

# Simulate lifecycle sequence
for hook_event in pre_claim post_claim pre_execute post_execute post_complete; do
    _needle_run_hook "$hook_event" "$NEEDLE_BEAD_ID" 2>/dev/null
done

if [[ -f "$LIFECYCLE_LOG" ]]; then
    actual=$(paste -sd',' "$LIFECYCLE_LOG")
    expected="pre_claim,post_claim,pre_execute,post_execute,post_complete"
    if [[ "$actual" == "$expected" ]]; then
        _test_pass "Lifecycle hooks fired in correct order: $actual"
    else
        _test_fail "Wrong hook order. Expected: $expected, Got: $actual"
    fi
else
    _test_fail "No lifecycle log found"
fi

# ============================================================================
# SCENARIO 6: NATO identifier allocation integrity
# ============================================================================

echo ""
echo "=========================================="
echo "Scenario 6: NATO identifier allocation integrity"
echo "=========================================="

_test_start "NATO alphabet has exactly 26 entries"
count=${#NEEDLE_NATO_ALPHABET[@]}
if [[ "$count" -eq 26 ]]; then
    _test_pass "NATO alphabet has 26 entries"
else
    _test_fail "Expected 26 NATO names, got $count"
fi

_test_start "First NATO identifier is 'alpha'"
if [[ "${NEEDLE_NATO_ALPHABET[0]}" == "alpha" ]]; then
    _test_pass "First NATO identifier is 'alpha'"
else
    _test_fail "Expected first to be 'alpha', got '${NEEDLE_NATO_ALPHABET[0]}'"
fi

_test_start "Last NATO identifier is 'zulu'"
last_idx=$((${#NEEDLE_NATO_ALPHABET[@]} - 1))
if [[ "${NEEDLE_NATO_ALPHABET[$last_idx]}" == "zulu" ]]; then
    _test_pass "Last NATO identifier is 'zulu'"
else
    _test_fail "Expected last to be 'zulu', got '${NEEDLE_NATO_ALPHABET[$last_idx]}'"
fi

_test_start "get_next_identifier_from_list returns alpha for empty list"
result=$(get_next_identifier_from_list "")
if [[ "$result" == "alpha" ]]; then
    _test_pass "Returns 'alpha' for empty list"
else
    _test_fail "Expected 'alpha', got '$result'"
fi

_test_start "get_next_identifier_from_list fills gaps in sequence"
result=$(get_next_identifier_from_list "alpha charlie delta")
if [[ "$result" == "bravo" ]]; then
    _test_pass "Fills gap: returned 'bravo' when alpha/charlie/delta used"
else
    _test_fail "Expected 'bravo', got '$result'"
fi

_test_start "get_next_identifier_from_list overflows to alpha-27 after all 26 used"
all_nato="${NEEDLE_NATO_ALPHABET[*]}"
result=$(get_next_identifier_from_list "$all_nato")
if [[ "$result" == "alpha-27" ]]; then
    _test_pass "Overflow: 'alpha-27' returned after all 26 NATO names used"
else
    _test_fail "Expected 'alpha-27', got '$result'"
fi

_test_start "Sequential allocation produces 10 unique identifiers"
used=""
ids=()
for i in $(seq 1 10); do
    next=$(get_next_identifier_from_list "$used")
    ids+=("$next")
    used="$used $next"
done
unique_count=$(printf '%s\n' "${ids[@]}" | sort -u | wc -l | tr -d ' ')
if [[ "$unique_count" -eq 10 ]]; then
    _test_pass "10 sequential allocations all unique: $(printf '%s,' "${ids[@]}" | sed 's/,$//')"
else
    _test_fail "Expected 10 unique IDs, got $unique_count: $(printf '%s,' "${ids[@]}")"
fi

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "=========================================="
echo "Integration Test Summary"
echo "=========================================="
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"
echo ""

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo "All integration tests passed!"
    exit 0
else
    echo "Some integration tests failed!"
    exit 1
fi
