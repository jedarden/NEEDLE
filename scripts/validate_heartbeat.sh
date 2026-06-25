#!/usr/bin/env bash
# Manual validation script for heartbeat functionality.
#
# This script demonstrates that:
# 1. Workers create heartbeat file on startup
# 2. File contains worker ID and last refresh timestamp
# 3. File updates every ~30 seconds
#
# Usage: ./scripts/validate_heartbeat.sh

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Get the heartbeat directory from config or use default
HEARTBEAT_DIR="${NEEDLE_HOME:-$HOME/.needle}/state/heartbeats"

log_info "Heartbeat directory: $HEARTBEAT_DIR"

# Check if heartbeat directory exists
if [[ ! -d "$HEARTBEAT_DIR" ]]; then
    log_error "Heartbeat directory does not exist: $HEARTBEAT_DIR"
    log_info "Start a NEEDLE worker first to create heartbeats"
    exit 1
fi

# Find heartbeat JSON files
HEARTBEAT_FILES=("$HEARTBEAT_DIR"/*.json)
if [[ ! -e "${HEARTBEAT_FILES[0]}" ]]; then
    log_error "No heartbeat files found in $HEARTBEAT_DIR"
    log_info "Start a NEEDLE worker first to create heartbeat files"
    exit 1
fi

log_info "Found ${#HEARTBEAT_FILES[@]} heartbeat file(s)"

# Analyze each heartbeat file
for hb_file in "${HEARTBEAT_FILES[@]}"; do
    log_info "================================"
    log_info "Analyzing: $(basename "$hb_file")"
    log_info "================================"

    # Check if file exists and is readable
    if [[ ! -r "$hb_file" ]]; then
        log_error "Cannot read file: $hb_file"
        continue
    fi

    # Extract fields using jq (or fallback to grep)
    if command -v jq &> /dev/null; then
        WORKER_ID=$(jq -r '.worker_id' "$hb_file")
        QUALIFIED_ID=$(jq -r '.qualified_id' "$hb_file")
        PID=$(jq -r '.pid' "$hb_file")
        LAST_HEARTBEAT=$(jq -r '.last_heartbeat' "$hb_file")
        BEADS_PROCESSED=$(jq -r '.beads_processed' "$hb_file")
        STATE=$(jq -r '.state' "$hb_file")
        WORKSPACE=$(jq -r '.workspace' "$hb_file")
    else
        # Fallback to grep/sed
        WORKER_ID=$(grep -o '"worker_id": "[^"]*"' "$hb_file" | cut -d'"' -f4)
        QUALIFIED_ID=$(grep -o '"qualified_id": "[^"]*"' "$hb_file" | cut -d'"' -f4)
        PID=$(grep -o '"pid": [0-9]*' "$hb_file" | cut -d' ' -f3)
        LAST_HEARTBEAT=$(grep -o '"last_heartbeat": "[^"]*"' "$hb_file" | cut -d'"' -f4)
        BEADS_PROCESSED=$(grep -o '"beads_processed": [0-9]*' "$hb_file" | cut -d' ' -f3)
        STATE=$(grep -o '"state": "[^"]*"' "$hb_file" | cut -d'"' -f4)
        WORKSPACE=$(grep -o '"workspace": "[^"]*"' "$hb_file" | cut -d'"' -f4)
    fi

    log_info "Worker ID: $WORKER_ID"
    log_info "Qualified ID: $QUALIFIED_ID"
    log_info "PID: $PID"
    log_info "State: $STATE"
    log_info "Workspace: $WORKSPACE"
    log_info "Beads Processed: $BEADS_PROCESSED"
    log_info "Last Heartbeat: $LAST_HEARTBEAT"

    # Validate fields
    if [[ -z "$WORKER_ID" ]]; then
        log_error "Missing or empty worker_id field"
    else
        log_info "✓ worker_id field present"
    fi

    if [[ -z "$QUALIFIED_ID" ]]; then
        log_error "Missing or empty qualified_id field"
    else
        log_info "✓ qualified_id field present"
    fi

    if [[ -z "$LAST_HEARTBEAT" ]]; then
        log_error "Missing or empty last_heartbeat field"
    else
        log_info "✓ last_heartbeat field present"

        # Check if timestamp is recent (within 2 minutes)
        if [[ "$LAST_HEARTBEAT" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2} ]]; then
            # Convert to epoch seconds (works on GNU date)
            if date --version &> /dev/null; then
                HB_EPOCH=$(date -d "$LAST_HEARTBEAT" +%s)
                NOW_EPOCH=$(date +%s)
                AGE=$((NOW_EPOCH - HB_EPOCH))

                if [[ $AGE -lt 120 ]]; then
                    log_info "✓ Timestamp is recent (${AGE}s ago)"
                else
                    log_warn "⚠ Timestamp is old (${AGE}s ago) - worker may be stale"
                fi
            else
                log_info "✓ Timestamp format is valid (skipping age check on BSD date)"
            fi
        else
            log_error "Invalid timestamp format: $LAST_HEARTBEAT"
        fi
    fi

    # Check if PID is alive
    if [[ -n "$PID" ]] && [[ "$PID" != "null" ]] && [[ "$PID" != "0" ]]; then
        if kill -0 "$PID" 2>/dev/null; then
            log_info "✓ Process $PID is alive"
        else
            log_warn "⚠ Process $PID is not running (stale heartbeat)"
        fi
    fi

    echo ""
done

log_info "================================"
log_info "Validation complete!"
log_info "================================"
log_info ""
log_info "To watch heartbeat files in real-time, run:"
log_info "  watch -n 5 'ls -la $HEARTBEAT_DIR/*.json && echo \"---\" && for f in $HEARTBEAT_DIR/*.json; do echo \"\$(basename \$f):\"; jq -c \"{worker_id, qualified_id, last_heartbeat}\" \"\$f\"; done'"
log_info ""
log_info "To monitor a single heartbeat file for changes:"
log_info "  watch -n 2 'cat $HEARTBEAT_DIR/<worker-id>.json | jq -c \"{worker_id, last_heartbeat, beads_processed}\"'"
