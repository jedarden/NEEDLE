#!/usr/bin/env bash
# Fleet validation script for needle-44e7e5cd: Stale assignee accumulation fix
#
# This script performs a fleet-wide sweep to count open+assigned beads,
# which should stay near zero after the fix is deployed.
#
# Usage:
#   ./scripts/fleet-stale-assignee-check.sh [baseline_file]
#
# Arguments:
#   baseline_file - Optional path to save baseline data for comparison
#
# Output:
#   - Human-readable summary to stdout
#   - Machine-readable JSON to specified file (if provided)

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# Find all bead workspaces
WORKSPACE_ROOT="${WORKSPACE_ROOT:-$HOME}"
OUTPUT_FILE="${1:-}"

# Temporary file for collecting results
TMP_RESULTS=$(mktemp)
trap "rm -f $TMP_RESULTS" EXIT

echo "Fleet-wide stale assignee check (needle-44e7e5cd validation)"
echo "============================================================"
echo "Scanning workspaces under: $WORKSPACE_ROOT"
echo ""

total_workspaces=0
total_beads=0
stale_count=0
problem_workspaces=0
declare -a workspace_details

# Scan for .beads directories
while IFS= read -r -d '' beads_dir; do
    workspace=$(dirname "$beads_dir")
    workspace_name=$(basename "$workspace")

    # Check if this is a bead-rs workspace
    if [[ ! -f "$beads_dir/config.json" ]]; then
        continue
    fi

    total_workspaces=$((total_workspaces + 1))

    # Count open+assigned beads for this workspace
    # Using sqlite3 to query the beads database directly
    db_path="$beads_dir/beads.db"

    if [[ ! -f "$db_path" ]]; then
        continue
    fi

    # Count open beads with assignees (the stale condition)
    local_stale=0
    local_total=0

    if sqlite3 "$db_path" > /dev/null 2>&1 <<EOF
SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NOT NULL AND assignee != '';
EOF
    then
        local_stale=$(sqlite3 "$db_path" "SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NOT NULL AND assignee != '';")
        local_total=$(sqlite3 "$db_path" "SELECT COUNT(*) FROM issues;")
    fi

    total_beads=$((total_beads + local_total))
    stale_count=$((stale_count + local_stale))

    if [[ $local_stale -gt 0 ]]; then
        problem_workspaces=$((problem_workspaces + 1))
        workspace_details+=("$workspace_name: $local_stale stale assignee(s)")
    fi

    # Save to temp file for JSON output
    echo "$workspace_name,$local_stale,$local_total" >> "$TMP_RESULTS"

done < <(find "$WORKSPACE_ROOT" -type d -name ".beads" -print0 2>/dev/null)

# Print human-readable summary
echo "Scan complete."
echo ""
echo "Results:"
echo "--------"
echo "Workspaces scanned: $total_workspaces"
echo "Total beads: $total_beads"
echo "Open+assigned beads (stale): $stale_count"
echo "Workspaces with stale beads: $problem_workspaces"
echo ""

# Health check
if [[ $stale_count -eq 0 ]]; then
    echo -e "${GREEN}✓ HEALTHY: No stale assignees found${NC}"
    echo "The fix for needle-44e7e5cd is working correctly."
elif [[ $stale_count -lt 50 ]]; then
    echo -e "${YELLOW}⚠ WARNING: $stale_count stale assignee(s) found${NC}"
    echo "This is within acceptable baseline but should be monitored."
    if [[ ${#workspace_details[@]} -gt 0 ]]; then
        echo ""
        echo "Affected workspaces:"
        for detail in "${workspace_details[@]}"; do
            echo "  - $detail"
        done
    fi
else
    echo -e "${RED}✗ CRITICAL: $stale_count stale assignees found!${NC}"
    echo "This exceeds the baseline threshold and may indicate the fix is not working."
    echo ""
    echo "Affected workspaces:"
    for detail in "${workspace_details[@]:0:10}"; do
        echo "  - $detail"
    done
    if [[ ${#workspace_details[@]} -gt 10 ]]; then
        echo "  ... and $((problem_workspaces - 10)) more"
    fi
fi

# Save JSON output if requested
if [[ -n "$OUTPUT_FILE" ]]; then
    cat > "$OUTPUT_FILE" <<EOF
{
  "scan_time": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "needle_ref": "needle-44e7e5cd",
  "summary": {
    "workspaces_scanned": $total_workspaces,
    "total_beads": $total_beads,
    "stale_assignee_count": $stale_count,
    "problem_workspaces": $problem_workspaces
  },
  "health": "$([[ $stale_count -lt 50 ]] && echo "healthy" || echo "critical")",
  "workspaces": [
EOF

    first=true
    while IFS=, read -r workspace_name local_stale local_total; do
        if [[ "$first" == "true" ]]; then
            first=false
        else
            echo "," >> "$OUTPUT_FILE"
        fi
        cat >> "$OUTPUT_FILE" <<EOF
    {
      "name": "$workspace_name",
      "stale_assignees": $local_stale,
      "total_beads": $local_total
    }
EOF
    done < "$TMP_RESULTS"

    echo "" >> "$OUTPUT_FILE"
    echo "  ]" >> "$OUTPUT_FILE"
    echo "}" >> "$OUTPUT_FILE"

    echo ""
    echo "JSON output saved to: $OUTPUT_FILE"
fi

# Exit with appropriate code
if [[ $stale_count -ge 50 ]]; then
    exit 1
elif [[ $stale_count -gt 0 ]]; then
    exit 2  # Warning exit code
else
    exit 0
fi
