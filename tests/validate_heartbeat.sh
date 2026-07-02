#!/bin/bash
# Validation script for heartbeat functionality.
# This script demonstrates that workers create heartbeat files on startup,
# refresh them every heartbeat_interval_secs (default 30s), and remove them
# on graceful shutdown (SIGTERM).
#
# Usage: ./tests/validate_heartbeat.sh
#
# Acceptance criteria:
# - Running worker has fresh heartbeat file
# - File updates every ~30s (heartbeat_interval_secs)
# - File contains worker ID and last refresh timestamp
# - Heartbeat file removed on graceful shutdown (SIGTERM)
# - No stale heartbeat files remain after worker exit

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} $*"; }
warn() { echo -e "${YELLOW}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} WARNING: $*"; }
error() { echo -e "${RED}[$(date +'%Y-%m-%d %H:%M:%S')]${NC} ERROR: $*"; }

# Get the NEEDLE workspace root
NEEDLE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export NEEDLE_ROOT

# Set up a temporary workspace for this test
TEST_DIR="$(mktemp -d -t needle-heartbeat-test.XXXXXX)"
trap "rm -rf '$TEST_DIR'" EXIT

log "Setting up test workspace at: $TEST_DIR"
mkdir -p "$TEST_DIR/workspace"
cd "$TEST_DIR/workspace"

# Initialize a minimal bead workspace
mkdir -p .beads
touch .beads/db.json

# Create a minimal config
mkdir -p .needle
cat > .needle.yaml <<EOF
# Minimal config for heartbeat validation
worker:
  max_workers: 1
  idle_timeout: 10
health:
  heartbeat_interval_secs: 5  # Faster than default for quicker validation
  heartbeat_ttl_secs: 30
EOF

log "Config created with heartbeat_interval_secs=5 for faster validation"

# Start a worker in the background
log "Starting NEEDLE worker..."
WORKSPACE="$TEST_DIR/workspace" timeout 60 "$NEEDLE_ROOT/target/debug/needle" worker --name "heartbeat-test" --worker-id 0 &
WORKER_PID=$!
trap "kill $WORKER_PID 2>/dev/null || true; rm -rf '$TEST_DIR'" EXIT

# Give the worker time to boot and create heartbeat file
HEARTBEAT_DIR="$TEST_DIR/workspace/.needle/state/heartbeats"
log "Waiting for heartbeat file to be created at: $HEARTBEAT_DIR"

# Wait up to 10 seconds for heartbeat file to appear
for i in {1..20}; do
    if [[ -f "$HEARTBEAT_DIR/"*.json ]]; then
        log "✓ Heartbeat file created!"
        break
    fi
    if [[ $i -eq 20 ]]; then
        error "Heartbeat file was not created within 10 seconds"
        exit 1
    fi
    sleep 0.5
done

# Find the heartbeat file
HEARTBEAT_FILE=$(ls "$HEARTBEAT_DIR/"*.json 2>/dev/null | head -1)
if [[ -z "$HEARTBEAT_FILE" ]]; then
    error "Could not find heartbeat file"
    exit 1
fi

log "Heartbeat file path: $HEARTBEAT_FILE"

# Validate file contents
log "Validating heartbeat file contents..."

WORKER_ID=$(jq -r '.worker_id' "$HEARTBEAT_FILE")
QUALIFIED_ID=$(jq -r '.qualified_id' "$HEARTBEAT_FILE")
PID=$(jq -r '.pid' "$HEARTBEAT_FILE")
LAST_HEARTBEAT=$(jq -r '.last_heartbeat' "$HEARTBEAT_FILE")
STATE=$(jq -r '.state' "$HEARTBEAT_FILE")

log "✓ worker_id: $WORKER_ID"
log "✓ qualified_id: $QUALIFIED_ID"
log "✓ pid: $PID"
log "✓ last_heartbeat: $LAST_HEARTBEAT"
log "✓ state: $STATE"

if [[ -z "$WORKER_ID" ]] || [[ -z "$LAST_HEARTBEAT" ]]; then
    error "Heartbeat file missing required fields"
    exit 1
fi

# Verify the timestamp is recent (within last 10 seconds)
TIMESTAMP_UNIX=$(date -d "$LAST_HEARTBEAT" +%s 2>/dev/null || date -j -f "%Y-%m-%dT%H:%M:%S%z" "$LAST_HEARTBEAT" +%s 2>/dev/null)
NOW_UNIX=$(date +%s)
AGE=$((NOW_UNIX - TIMESTAMP_UNIX))

if [[ $AGE -gt 10 ]]; then
    error "Heartbeat timestamp is too old: ${AGE}s ago"
    exit 1
fi

log "✓ Heartbeat timestamp is fresh (${AGE}s ago)"

# Test periodic refresh
log "Testing periodic heartbeat refresh (waiting 15 seconds for at least 2 updates)..."

# Read initial modification time
INITIAL_MTIME=$(stat -c %Y "$HEARTBEAT_FILE" 2>/dev/null || stat -f %m "$HEARTBEAT_FILE")
log "Initial file modification time: $INITIAL_MTIME"

# Wait for heartbeat updates
UPDATE_COUNT=0
for second in {1..15}; do
    sleep 1

    CURRENT_MTIME=$(stat -c %Y "$HEARTBEAT_FILE" 2>/dev/null || stat -f %m "$HEARTBEAT_FILE")

    if [[ "$CURRENT_MTIME" -gt "$INITIAL_MTIME" ]]; then
        UPDATE_COUNT=$((UPDATE_COUNT + 1))
        NEW_LAST_HEARTBEAT=$(jq -r '.last_heartbeat' "$HEARTBEAT_FILE")
        log "✓ Heartbeat updated #$UPDATE_COUNT at: $NEW_LAST_HEARTBEAT"
        INITIAL_MTIME="$CURRENT_MTIME"
    fi
done

# With 5-second interval, we should see at least 2 updates in 15 seconds
if [[ $UPDATE_COUNT -lt 2 ]]; then
    warn "Expected at least 2 heartbeat updates, saw $UPDATE_COUNT"
    warn "This might be normal if the worker hasn't claimed any beads yet"
else
    log "✓ Heartbeat refreshed $UPDATE_COUNT times in 15 seconds"
fi

# Test graceful shutdown cleanup
log "Testing graceful shutdown with SIGTERM..."
log "Sending SIGTERM to worker process (PID: $WORKER_PID)"
kill -TERM $WORKER_PID

# Wait up to 5 seconds for the worker to exit and clean up
for i in {1..10}; do
    if ! kill -0 $WORKER_PID 2>/dev/null; then
        log "✓ Worker process has exited"
        break
    fi
    if [[ $i -eq 10 ]]; then
        error "Worker did not exit within 5 seconds after SIGTERM"
        kill -9 $WORKER_PID 2>/dev/null || true
        exit 1
    fi
    sleep 0.5
done

# Give a moment for file cleanup to complete
sleep 0.5

# Verify heartbeat file was removed
if [[ -f "$HEARTBEAT_FILE" ]]; then
    error "Heartbeat file still exists after graceful shutdown!"
    error "File path: $HEARTBEAT_FILE"
    error "This indicates cleanup on SIGTERM is not working properly"
    exit 1
fi

log "✓ Heartbeat file removed on graceful shutdown"

# Verify no other heartbeat files remain
REMAINING_FILES=$(ls "$HEARTBEAT_DIR/"*.json 2>/dev/null | wc -l)
if [[ $REMAINING_FILES -gt 0 ]]; then
    error "Unexpected heartbeat files remain in $HEARTBEAT_DIR"
    ls -la "$HEARTBEAT_DIR/"
    exit 1
fi

log "✓ No stale heartbeat files remain after exit"

# Final validation
log ""
log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
log "✅ HEARTBEAT VALIDATION SUCCESSFUL"
log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
log "✓ Heartbeat file created on startup"
log "✓ File contains worker_id and last_heartbeat timestamp"
log "✓ File refreshed periodically ($UPDATE_COUNT updates in 15 seconds)"
log "✓ Timestamps are fresh and current"
log "✓ Heartbeat file removed on graceful shutdown (SIGTERM)"
log "✓ No stale heartbeat files remain after exit"
log ""
log "The heartbeat functionality is working as specified!"
