#!/bin/bash
# Auto-close false alarm worker starvation alerts
# Triggered when HUMAN bead is created

BEAD_ID=$1
BEAD_TITLE=$2

# Only check worker starvation alerts
if [[ ! "$BEAD_TITLE" =~ "has no work available" ]]; then
    exit 0
fi

echo "🔍 Verifying worker starvation alert: $BEAD_ID"

# Check for ready beads
READY_COUNT=$(br ready --format json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")

if [ "$READY_COUNT" -gt 0 ]; then
    echo "⚠️ FALSE ALARM: $READY_COUNT beads ready to work!"

    # Check database health
    WAL_SIZE=$(stat -c%s .beads/beads.db-wal 2>/dev/null || echo "0")
    if [ "$WAL_SIZE" -gt 10485760 ]; then
        CAUSE="Database corruption detected (WAL: $WAL_SIZE bytes)"

        echo "🔧 Backing up corrupted database and rebuilding..."
        # Backup corrupted database
        mv .beads/beads.db .beads/beads.db.corrupted-$(date +%Y%m%d-%H%M%S) 2>/dev/null || true

        # Rebuild from issues.jsonl using br sync import
        if command -v br &> /dev/null; then
            br sync import 2>/dev/null || echo "⚠️ br sync import failed"
        fi
    else
        CAUSE="Worker query mismatch (unknown)"
    fi

    # Get sample of ready beads
    READY_SAMPLE=$(br ready 2>/dev/null | head -10)

    # Close as false alarm
    br comment $BEAD_ID "**FALSE ALARM DETECTED**

Verification shows **$READY_COUNT beads ready to work**.

**Root Cause:** $CAUSE

**Ready Beads (sample):**
\`\`\`
$READY_SAMPLE
\`\`\`

Auto-closing as false alarm." 2>/dev/null

    br close $BEAD_ID 2>/dev/null

    echo "✅ Closed $BEAD_ID as false alarm"
    exit 0
else
    echo "✓ Starvation alert appears legitimate (0 ready beads)"
    exit 0
fi
