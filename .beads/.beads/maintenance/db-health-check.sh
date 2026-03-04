#!/bin/bash
# Database health check - prevents worker starvation false alarms
# Run periodically or before creating starvation alerts

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

    # Rebuild from JSONL source of truth
    br sync --rebuild -q

    echo "Database rebuilt successfully"
}

# Main execution
main() {
    cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

    if ! check_database_health; then
        echo "Database corruption detected!"
        rebuild_database
        return 0
    fi

    echo "Database healthy"
}

# Allow sourcing or direct execution
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
