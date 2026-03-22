#!/bin/bash
# E2E: Additive worker launch — collision avoidance
#
# Proves that launching workers when sessions already exist skips occupied
# NATO names and assigns the next available ones.
#
# Strategy:
#   - Create 2 fake tmux sessions (needle-test-agent-alpha, needle-test-agent-bravo)
#   - Run `needle run --count 2` against the same agent
#   - Verify the new sessions are charlie + delta (skipping alpha + bravo)
#   - Clean up all sessions
#
# Dependencies: tmux, needle binary (built from this repo)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── Color helpers ──────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

pass() { echo -e "  ${GREEN}PASS${NC}: $1"; }
fail() { echo -e "  ${RED}FAIL${NC}: $1"; PASS=false; }
info() { echo -e "  ${YELLOW}INFO${NC}: $1"; }

# ── Build needle ───────────────────────────────────────────────────────────────

echo "=== E2E: Additive Worker Launch — Collision Avoidance ==="
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

# ── Verify tmux ────────────────────────────────────────────────────────────────

if ! command -v tmux &>/dev/null; then
    echo "FATAL: tmux is required for this test"
    exit 1
fi

# ── Setup ──────────────────────────────────────────────────────────────────────

AGENT="e2e-collision-test"
SESSIONS_CREATED=()
PASS=true

cleanup() {
    for s in "${SESSIONS_CREATED[@]:-}"; do
        tmux kill-session -t "$s" 2>/dev/null || true
    done
}
trap cleanup EXIT

# ── Step 1: Create 2 fake "occupied" tmux sessions ──────────────────────────

echo "Step 1: Creating fake occupied sessions..."
for name in alpha bravo; do
    session="needle-${AGENT}-${name}"
    tmux new-session -d -s "$session" "sleep 120"
    SESSIONS_CREATED+=("$session")
    info "Created session: $session"
done

# Verify they exist.
EXISTING=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^needle-${AGENT}-" | sort)
EXPECTED_EXISTING=$(printf "needle-%s-alpha\nneedle-%s-bravo" "$AGENT" "$AGENT")
if [ "$EXISTING" = "$EXPECTED_EXISTING" ]; then
    pass "2 occupied sessions exist (alpha, bravo)"
else
    fail "Expected alpha+bravo sessions, got: $EXISTING"
fi

# ── Step 2: Launch 2 more workers — they should get charlie + delta ─────────

echo ""
echo "Step 2: Launching 2 additional workers..."

# We need a minimal config and workspace so needle run doesn't fail on config load.
TMPBASE="$(mktemp -d)"
FAKE_HOME="$TMPBASE/home"
WORKSPACE="$TMPBASE/workspace"

# Save real HOME for br discovery.
REAL_HOME="$HOME"
export HOME="$FAKE_HOME"
mkdir -p "$HOME/.config/needle/adapters" "$WORKSPACE"

BR_BIN="$(which br 2>/dev/null || echo "$REAL_HOME/.local/bin/br")"

# Init a minimal workspace.
(cd "$WORKSPACE" && "$BR_BIN" init 2>&1) || {
    echo "FATAL: br init failed"
    exit 1
}

# Create one bead so the workers have something (they'll just exit quickly).
(cd "$WORKSPACE" && "$BR_BIN" create --title "collision-test bead" --silent 2>/dev/null) || true

# Agent adapter — just close the bead immediately.
cat > "$HOME/.config/needle/adapters/${AGENT}.yaml" <<YAML
name: ${AGENT}
agent_cli: bash
invoke_template: "cd ${WORKSPACE} && $BR_BIN close {bead_id} --reason 'collision-test' 2>/dev/null || true"
timeout_secs: 10
YAML

# Needle config.
cat > "$HOME/.config/needle/config.yaml" <<YAML
worker:
  idle_action: exit
  max_workers: 10
  launch_stagger_seconds: 0
agent:
  default: ${AGENT}
  timeout: 10
health:
  heartbeat_interval_secs: 1
  heartbeat_ttl_secs: 10
YAML

# Run outside tmux (unset TMUX so launch_workers creates tmux sessions).
unset TMUX

OUTPUT=$("$NEEDLE_BIN" run \
    --workspace "$WORKSPACE" \
    --agent "$AGENT" \
    --count 2 2>&1) || {
    fail "needle run --count 2 failed: $OUTPUT"
}

echo "$OUTPUT"

# Track sessions for cleanup.
for name in charlie delta; do
    SESSIONS_CREATED+=("needle-${AGENT}-${name}")
done

# ── Step 3: Verify all 4 sessions exist with correct names ─────────────────

echo ""
echo "Step 3: Verifying session names..."

ALL_SESSIONS=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^needle-${AGENT}-" | sort)

for name in alpha bravo charlie delta; do
    session="needle-${AGENT}-${name}"
    if echo "$ALL_SESSIONS" | grep -q "^${session}$"; then
        pass "Session '${session}' exists"
    else
        fail "Session '${session}' missing"
    fi
done

# Verify no echo/foxtrot (shouldn't have over-launched).
for name in echo foxtrot; do
    session="needle-${AGENT}-${name}"
    if echo "$ALL_SESSIONS" | grep -q "^${session}$"; then
        fail "Unexpected session '${session}' — over-launched"
    fi
done

# ── Step 4: Verify the output mentions charlie + delta ─────────────────────

echo ""
echo "Step 4: Verifying launch output..."

if echo "$OUTPUT" | grep -q "charlie"; then
    pass "Output mentions 'charlie'"
else
    fail "Output does not mention 'charlie'"
fi

if echo "$OUTPUT" | grep -q "delta"; then
    pass "Output mentions 'delta'"
else
    fail "Output does not mention 'delta'"
fi

# ── Result ─────────────────────────────────────────────────────────────────────

echo ""
if [ "$PASS" = true ]; then
    echo -e "${GREEN}ALL ASSERTIONS PASSED${NC}"
    # Clean up tmpdir.
    rm -rf "$TMPBASE"
    exit 0
else
    echo -e "${RED}SOME ASSERTIONS FAILED${NC}"
    echo ""
    echo "=== All needle sessions ==="
    tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^needle-" | sort || true
    echo ""
    echo "=== Launch output ==="
    echo "$OUTPUT"
    rm -rf "$TMPBASE"
    exit 1
fi
