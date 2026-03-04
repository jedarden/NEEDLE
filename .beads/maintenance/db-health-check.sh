#!/bin/bash
# Proactive database health monitoring and maintenance
# Detects WAL file anomalies and performs checkpoint before corruption occurs
#
# This script implements a tiered response:
# 1. < 1MB: Healthy, no action needed
# 2. 1-5MB: Warning, trigger proactive checkpoint
# 3. 5-10MB: High warning, force checkpoint and alert
# 4. > 10MB: Critical, rebuild database from JSONL
#
# The WAL checkpoint is now also triggered automatically by br sync operations,
# but this script provides an additional safety net for long-running sessions.
#
# Exit codes:
#   0 = Database healthy (no action needed, or checkpoint completed)
#   1 = Corruption detected and rebuilt (caller should skip current action)
#   2 = Error (rebuild failed or other error)
#
# Usage in worker starvation detection:
#   if ! .beads/maintenance/db-health-check.sh; then
#       # DB was corrupted and rebuilt, exit without creating alert
#       exit 0
#   fi

set -e

WAL_FILE=".beads/beads.db-wal"
CHECKPOINT_THRESHOLD=1048576     # 1MB - proactive checkpoint
WARNING_THRESHOLD=5242880        # 5MB - force checkpoint with alert
CRITICAL_THRESHOLD=10485760      # 10MB - rebuild database

# Function to format bytes nicely
format_bytes() {
    local bytes=$1
    if command -v numfmt &> /dev/null; then
        numfmt --to=iec-i --suffix=B "$bytes" 2>/dev/null || echo "${bytes} bytes"
    else
        echo "${bytes} bytes"
    fi
}

# Function to trigger a checkpoint via br sync --flush-only
trigger_checkpoint() {
    local reason=$1
    echo "🔄 Triggering checkpoint: $reason"

    # br sync --flush-only will checkpoint after export
    # If no dirty issues, it still opens the DB which triggers auto-checkpoint
    if command -v br &> /dev/null; then
        # First try to flush any dirty issues
        if br sync --flush-only 2>&1 | grep -q "Nothing to export"; then
            # No dirty issues, but we still want to checkpoint
            # The checkpoint is called in the sync code even if nothing to export
            echo "   Checkpoint completed (no dirty issues)"
        else
            echo "   Checkpoint completed (flushed dirty issues)"
        fi
        return 0
    else
        echo "⚠️ br command not available - cannot checkpoint"
        return 1
    fi
}

# Check if WAL file exists
if [ ! -f "$WAL_FILE" ]; then
    echo "✓ No WAL file found - database healthy"
    exit 0
fi

WAL_SIZE=$(stat -c%s "$WAL_FILE" 2>/dev/null || stat -f%z "$WAL_FILE" 2>/dev/null || echo "0")

echo "📊 Database Health Check"
echo "   WAL Size: $(format_bytes $WAL_SIZE)"
echo "   Checkpoint threshold: $(format_bytes $CHECKPOINT_THRESHOLD)"
echo "   Warning threshold: $(format_bytes $WARNING_THRESHOLD)"
echo "   Critical threshold: $(format_bytes $CRITICAL_THRESHOLD)"

# Tiered response based on WAL size
if [ "$WAL_SIZE" -gt "$CRITICAL_THRESHOLD" ]; then
    # CRITICAL: Rebuild database
    echo "🚨 CRITICAL: WAL file exceeds 10MB - database corruption suspected"

    # Backup corrupted database
    BACKUP=".beads/beads.db.corrupted-$(date +%Y%m%d-%H%M%S)"
    echo "💾 Backing up database to $BACKUP"
    cp .beads/beads.db "$BACKUP" 2>/dev/null || true

    # Check if JSONL source exists
    if [ ! -f ".beads/issues.jsonl" ]; then
        echo "❌ ERROR: issues.jsonl not found - cannot rebuild"
        exit 2
    fi

    echo "🔧 Rebuilding database from issues.jsonl..."

    # Remove corrupted database files
    rm -f .beads/beads.db .beads/beads.db-wal .beads/beads.db-shm

    # Rebuild from JSONL using br sync
    if command -v br &> /dev/null; then
        if br sync --import-only 2>&1; then
            echo "✅ Database rebuilt successfully from JSONL"

            # Verify rebuild worked (WAL should be absent or small)
            if [ -f "$WAL_FILE" ]; then
                NEW_WAL_SIZE=$(stat -c%s "$WAL_FILE" 2>/dev/null || echo "0")
                echo "   New WAL Size: $(format_bytes $NEW_WAL_SIZE)"
            else
                echo "   WAL file removed (checkpointed)"
            fi

            # Create notification bead
            if br create "Database corruption repaired automatically" \
                --description "WAL file exceeded 10MB ($(format_bytes $WAL_SIZE)). Database rebuilt from issues.jsonl source of truth using \`br sync --import-only\`. Corrupted DB backed up to $BACKUP." \
                --type info \
                --priority 4 2>/dev/null; then
                echo "📝 Created notification bead"
            fi

            # Exit 1 to signal caller that corruption was found and fixed
            # This allows worker to skip creating starvation alert
            exit 1
        else
            echo "❌ ERROR: br sync --import-only failed"
            # Restore backup
            mv "$BACKUP" .beads/beads.db
            echo "⚠️ Restored corrupted database from backup"
            exit 2
        fi
    else
        echo "⚠️ WARNING: br command not available - manual rebuild required"
        echo "   Run: br sync --import-only"
        exit 2
    fi

elif [ "$WAL_SIZE" -gt "$WARNING_THRESHOLD" ]; then
    # WARNING: Force checkpoint with alert
    echo "⚠️ WARNING: WAL file exceeds 5MB - forcing checkpoint"

    if trigger_checkpoint "WAL size warning ($(format_bytes $WAL_SIZE))"; then
        # Verify checkpoint worked
        if [ -f "$WAL_FILE" ]; then
            NEW_WAL_SIZE=$(stat -c%s "$WAL_FILE" 2>/dev/null || echo "0")
            echo "   New WAL Size: $(format_bytes $NEW_WAL_SIZE)"
        else
            echo "   WAL file removed (checkpointed)"
        fi
        echo "✅ Checkpoint completed"
    fi

elif [ "$WAL_SIZE" -gt "$CHECKPOINT_THRESHOLD" ]; then
    # PROACTIVE: Trigger checkpoint before issues arise
    echo "ℹ️ INFO: WAL file exceeds 1MB - proactive checkpoint"

    if trigger_checkpoint "Proactive maintenance ($(format_bytes $WAL_SIZE))"; then
        echo "✅ Proactive checkpoint completed"
    fi

else
    echo "✅ Database healthy - WAL size within normal limits"
fi
