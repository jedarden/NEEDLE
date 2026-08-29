#!/usr/bin/env bash
# Cleanup superseded checkpoint objects from git tracking
#
# This script removes checkpoint objects that are no longer referenced by
# current.json or previous.json from git tracking. The objects remain in
# git history for recovery, but are removed from the working tree.
#
# Usage: ./scripts/cleanup-superseded-checkpoint-objects.sh
#
# This is called automatically by scripts/commit-checkpoint.sh before committing.

set -euo pipefail

CHECKPOINT_DIR="$(git rev-parse --show-toplevel)/.beads/checkpoint"

# Verify we're in a git repo
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    echo "Error: Not in a git repository" >&2
    exit 1
fi

# Verify checkpoint directory exists
if [ ! -d "$CHECKPOINT_DIR" ]; then
    echo "Error: Checkpoint directory not found: $CHECKPOINT_DIR" >&2
    exit 1
fi

cd "$CHECKPOINT_DIR"

# Function to extract active_root.path from a checkpoint JSON file
extract_active_root() {
    local file="$1"
    if [ ! -f "$file" ]; then
        echo "Error: Checkpoint file not found: $file" >&2
        exit 1
    fi

    # Extract active_root.path using jq
    jq -r '.active_root.path // empty' "$file"
}

# Extract active root paths from both checkpoint files
CURRENT_ROOT=$(extract_active_root "current.json")
PREVIOUS_ROOT=$(extract_active_root "previous.json")

if [ -z "$CURRENT_ROOT" ] || [ -z "$PREVIOUS_ROOT" ]; then
    echo "Error: Failed to extract active_root paths" >&2
    exit 1
fi

echo "Superseded checkpoint object cleanup"
echo "===================================="
echo "Active objects (will be kept):"
echo "  - $CURRENT_ROOT"
echo "  - $PREVIOUS_ROOT"
echo ""

# Build list of active object filenames (just the filename, not the full path)
ACTIVE_FILES=(
    "$(basename "$CURRENT_ROOT")"
    "$(basename "$PREVIOUS_ROOT")"
)

# Find all tracked objects in the checkpoint objects directory
TRACKED_OBJECTS=$(git ls-files '.beads/checkpoint/objects/' 2>/dev/null || true)

if [ -z "$TRACKED_OBJECTS" ]; then
    echo "No tracked checkpoint objects found - nothing to clean up"
    exit 0
fi

# Count objects before cleanup
TOTAL_BEFORE=$(echo "$TRACKED_OBJECTS" | wc -l)
echo "Tracked checkpoint objects: $TOTAL_BEFORE"

# Find and remove superseded objects
REMOVED_COUNT=0
while IFS= read -r object; do
    # Check if this object is in the active set
    IS_ACTIVE=false
    for active_file in "${ACTIVE_FILES[@]}"; do
        if [[ "$object" == *"$active_file" ]]; then
            IS_ACTIVE=true
            break
        fi
    done

    if [ "$IS_ACTIVE" = false ]; then
        echo "  Removing superseded: $object"
        git rm "$object" 2>/dev/null || true
        ((REMOVED_COUNT++))
    fi
done <<< "$TRACKED_OBJECTS"

echo ""
echo "Cleanup summary:"
echo "  Total tracked before: $TOTAL_BEFORE"
echo "  Removed: $REMOVED_COUNT"
echo "  Remaining active: 2"
echo ""
echo "✓ Superseded checkpoint object cleanup completed"
