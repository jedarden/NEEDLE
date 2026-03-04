#!/bin/bash
# Auto-close false alarm worker starvation alerts
# Triggered when HUMAN bead is created
#
# This hook:
# 1. Verifies starvation claim by checking for ready beads
# 2. Uses the shared db-health-check.sh for database diagnostics
# 3. Auto-closes false alarms with detailed diagnosis

BEAD_ID=$1
BEAD_TITLE=$2

# Only check worker starvation alerts
if [[ ! "$BEAD_TITLE" =~ "has no work available" ]]; then
    exit 0
fi

echo "🔍 Verifying worker starvation alert: $BEAD_ID"

# First, run database health check to catch corruption early
# This will auto-rebuild if needed
HEALTH_SCRIPT="$(dirname "$0")/../maintenance/db-health-check.sh"
if [[ -x "$HEALTH_SCRIPT" ]]; then
    echo "Running database health check..."
    if ! "$HEALTH_SCRIPT"; then
        # Health check returned 1 = corruption was detected and rebuilt
        # or 2 = error occurred
        echo "Database health issue detected and addressed"
    fi
fi

# Check for ready beads
READY_COUNT=$(br ready --format json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")

if [ "$READY_COUNT" -gt 0 ]; then
    echo "⚠️ FALSE ALARM: $READY_COUNT beads ready to work!"

    # Check database health for root cause analysis
    WAL_SIZE=$(stat -c%s .beads/beads.db-wal 2>/dev/null || echo "0")
    if [ "$WAL_SIZE" -gt 10485760 ]; then
        CAUSE="Database corruption detected (WAL: $WAL_SIZE bytes > 10MB threshold)"
    else
        CAUSE="Worker query mismatch (database healthy)"
    fi

    # Get sample of ready beads
    READY_SAMPLE=$(br ready 2>/dev/null | head -10)

    # Close as false alarm
    br comments add $BEAD_ID "**FALSE ALARM DETECTED**

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
