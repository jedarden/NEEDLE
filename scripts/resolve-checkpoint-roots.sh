#!/usr/bin/env bash
# resolve-checkpoint-roots.sh
# Extract active_root object IDs from checkpoint pointers and output their full paths
# Usage: scripts/resolve-checkpoint-roots.sh [current.json] [previous.json]
# Exit codes: 0 = success, 1 = error (malformed pointer or missing object)

set -euo pipefail

# Allow override via command-line arguments
CURRENT_FILE="${1:-.beads/checkpoint/current.json}"
PREVIOUS_FILE="${2:-.beads/checkpoint/previous.json}"
CHECKPOINT_DIR="$(dirname "$CURRENT_FILE")"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

# Function to extract active_root path from a checkpoint file
extract_active_root() {
    local file="$1"

    if [[ ! -f "$file" ]]; then
        echo -e "${RED}Error: Checkpoint file not found: $file${NC}" >&2
        return 1
    fi

    # Extract active_root.path using jq
    local root_path
    root_path=$(jq -r '.active_root.path // empty' "$file" 2>/dev/null)

    if [[ -z "$root_path" ]]; then
        echo -e "${RED}Error: No active_root.path found in $file${NC}" >&2
        return 1
    fi

    # Validate path format (should be objects/<hash>.jsonl)
    if [[ ! "$root_path" =~ ^objects/[a-f0-9]+\.jsonl$ ]]; then
        echo -e "${RED}Error: Malformed active_root.path in $file: $root_path${NC}" >&2
        return 1
    fi

    echo "$root_path"
}

# Function to verify object file exists
verify_object_exists() {
    local obj_path="$1"
    local full_path="$CHECKPOINT_DIR/$obj_path"

    if [[ ! -f "$full_path" ]]; then
        echo -e "${RED}Error: Object file not found: $full_path${NC}" >&2
        return 1
    fi

    return 0
}

# Main execution
main() {
    local errors=0
    local current_root
    local previous_root

    # Extract active_root paths
    echo "Extracting active_root paths from checkpoint files..."

    if ! current_root=$(extract_active_root "$CURRENT_FILE"); then
        errors=$((errors + 1))
    fi

    if ! previous_root=$(extract_active_root "$PREVIOUS_FILE"); then
        errors=$((errors + 1))
    fi

    # If extraction failed, exit with error
    if [[ $errors -gt 0 ]]; then
        exit 1
    fi

    # Output full paths
    echo "Current active root: $CHECKPOINT_DIR/$current_root"
    echo "Previous active root: $CHECKPOINT_DIR/$previous_root"

    # Verify objects exist
    echo "Verifying object files exist..."

    if ! verify_object_exists "$current_root"; then
        errors=$((errors + 1))
    fi

    if ! verify_object_exists "$previous_root"; then
        errors=$((errors + 1))
    fi

    # Final status
    if [[ $errors -gt 0 ]]; then
        echo -e "${RED}Failed: $errors error(s) encountered${NC}" >&2
        exit 1
    fi

    echo -e "${GREEN}Success: Both checkpoint roots resolved successfully${NC}"

    # Output just the paths (for scripting use)
    echo ""
    echo "$CHECKPOINT_DIR/$current_root"
    echo "$CHECKPOINT_DIR/$previous_root"

    exit 0
}

main "$@"
