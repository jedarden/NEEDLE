#!/bin/bash
# Proactive database health monitoring and repair
# Detects WAL file anomalies and rebuilds database from JSONL source of truth

set -e

WAL_FILE=".beads/beads.db-wal"
THRESHOLD=10485760  # 10MB

# Check if WAL file exists
if [ ! -f "$WAL_FILE" ]; then
    echo "✓ No WAL file found - database healthy"
    exit 0
fi

WAL_SIZE=$(stat -c%s "$WAL_FILE" 2>/dev/null || stat -f%z "$WAL_FILE" 2>/dev/null || echo "0")

echo "📊 Database Health Check"
echo "   WAL Size: $(numfmt --to=iec-i --suffix=B $WAL_SIZE 2>/dev/null || echo "${WAL_SIZE} bytes")"
echo "   Threshold: $(numfmt --to=iec-i --suffix=B $THRESHOLD 2>/dev/null || echo "${THRESHOLD} bytes")"

if [ "$WAL_SIZE" -gt "$THRESHOLD" ]; then
    echo "⚠️ WAL file exceeds threshold - database corruption suspected"

    # Backup corrupted database
    BACKUP=".beads/beads.db.corrupted-$(date +%Y%m%d-%H%M%S)"
    echo "💾 Backing up database to $BACKUP"
    cp .beads/beads.db "$BACKUP" 2>/dev/null || true

    # Check if JSONL source exists
    if [ ! -f ".beads/issues.jsonl" ]; then
        echo "❌ ERROR: issues.jsonl not found - cannot rebuild"
        exit 1
    fi

    echo "🔧 Rebuilding database from issues.jsonl..."

    # Remove corrupted database files
    rm -f .beads/beads.db .beads/beads.db-wal .beads/beads.db-shm

    # Rebuild from JSONL using br sync
    if command -v br &> /dev/null; then
        if br sync import 2>&1; then
            echo "✅ Database rebuilt successfully from JSONL"

            # Verify rebuild worked
            NEW_WAL_SIZE=$(stat -c%s .beads/beads.db-wal 2>/dev/null || echo "0")
            echo "   New WAL Size: $(numfmt --to=iec-i --suffix=B $NEW_WAL_SIZE 2>/dev/null || echo "${NEW_WAL_SIZE} bytes")"

            # Create notification bead
            if br create "Database corruption repaired automatically" \
                --description "WAL file exceeded 10MB ($WAL_SIZE bytes). Database rebuilt from issues.jsonl source of truth using \`br sync import\`. Corrupted DB backed up to $BACKUP." \
                --type info \
                --priority 4 2>/dev/null; then
                echo "📝 Created notification bead"
            fi
        else
            echo "❌ ERROR: br sync import failed"
            # Restore backup
            mv "$BACKUP" .beads/beads.db
            echo "⚠️ Restored corrupted database from backup"
            exit 1
        fi
    else
        echo "⚠️ WARNING: br command not available - manual rebuild required"
        echo "   Run: br sync import"
        exit 1
    fi
else
    echo "✅ Database healthy - WAL size within normal limits"
fi
