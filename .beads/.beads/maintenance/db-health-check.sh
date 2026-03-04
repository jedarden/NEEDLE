#!/bin/bash
# Database health check - prevents worker starvation false alarms
# Run periodically or before creating starvation alerts
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

BEADS_DIR="${BEADS_DIR:-.beads}"
WAL_FILE="$BEADS_DIR/beads.db-wal"
THRESHOLD=10485760  # 10MB

check_database_health() {
    if [[ ! -f "$WAL_FILE" ]]; then
        return 0  # No WAL file = healthy (or no DB yet)
    fi

    local wal_size=$(stat -c%s "$WAL_FILE" 2>/dev/null || echo "0")

    if [[ "$wal_size" -gt "$THRESHOLD" ]]; then
        echo "CORRUPT: WAL file too large: $wal_size bytes (threshold: $THRESHOLD)"
        return 1
    fi

    return 0
}

verify_ready_beads() {
    local ready_count=$(br ready --format json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
    echo "$ready_count"
}

rebuild_database() {
    echo "Rebuilding database from JSONL..."

    # Backup corrupted database
    local backup="$BEADS_DIR/beads.db.corrupted-$(date +%Y%m%d-%H%M%S)"
    [[ -f "$BEADS_DIR/beads.db" ]] && mv "$BEADS_DIR/beads.db" "$backup"
    [[ -f "$WAL_FILE" ]] && rm -f "$WAL_FILE"
    [[ -f "$BEADS_DIR/beads.db-shm" ]] && rm -f "$BEADS_DIR/beads.db-shm"

    # Rebuild from JSONL source of truth
    if ! br sync --import-only 2>&1; then
        echo "ERROR: Failed to rebuild database"
        # Try to restore backup
        if [[ -f "$backup" ]]; then
            mv "$backup" "$BEADS_DIR/beads.db"
            echo "Restored corrupted database from backup"
        fi
        return 1
    fi

    echo "Database rebuilt successfully"
    return 0
}

# Main execution
main() {
    cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

    if ! check_database_health; then
        echo "Database corruption detected!"
        if rebuild_database; then
            # Return 1 to signal caller that corruption was found and fixed
            # This allows worker to skip creating starvation alert
            return 1
        else
            # Return 2 to signal error (rebuild failed)
            return 2
        fi
    fi

    echo "Database healthy"
    return 0
}

# Allow sourcing or direct execution
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
