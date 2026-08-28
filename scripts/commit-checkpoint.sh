#!/usr/bin/env bash
# Commit checkpoint with active root objects
#
# This script ensures that every commit of .beads/checkpoint/ includes the objects
# referenced by current.json and previous.json active_root fields, atomically with
# the pointer files themselves.
#
# Usage: scripts/commit-checkpoint.sh [commit-message]
#

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

BEAD_DIR=".beads/checkpoint"
CURRENT_JSON="$BEAD_DIR/current.json"
PREVIOUS_JSON="$BEAD_DIR/previous.json"

# Flush checkpoint to ensure we have the latest state
echo "Flushing checkpoint..."
bead sync flush-only > /dev/null

# Use the root resolver script to extract and verify active root paths
echo "Resolving active root objects..."
RESOLVER_OUTPUT="$(scripts/resolve-checkpoint-roots.sh "$CURRENT_JSON" "$PREVIOUS_JSON")"
RESOLVER_EXIT=$?

if [[ $RESOLVER_EXIT -ne 0 ]]; then
    echo "Error: Failed to resolve checkpoint roots" >&2
    echo "$RESOLVER_OUTPUT" >&2
    exit 1
fi

# Extract just the path components (last two lines are the full paths)
CURRENT_ROOT_FULL=$(echo "$RESOLVER_OUTPUT" | tail -2 | head -1)
PREVIOUS_ROOT_FULL=$(echo "$RESOLVER_OUTPUT" | tail -1)

# Extract relative paths from full paths
CURRENT_ROOT="${CURRENT_ROOT_FULL#$BEAD_DIR/}"
PREVIOUS_ROOT="${PREVIOUS_ROOT_FULL#$BEAD_DIR/}"

echo "Current root: $CURRENT_ROOT"
echo "Previous root: $PREVIOUS_ROOT"

# Stage the pointer files and their referenced objects
git add "$CURRENT_JSON" "$PREVIOUS_JSON" "$BEAD_DIR/forensic.jsonl"
git add "$BEAD_DIR/$CURRENT_ROOT" "$BEAD_DIR/$PREVIOUS_ROOT"

# Check for superseded objects that are tracked but should be removed
# These are objects listed in deleted_paths that are still in git
TRACKED_OBJECTS=$(git ls-files "$BEAD_DIR/objects/")
DELETED_PATHS=$(jq -r '.deleted_paths[]' "$CURRENT_JSON" "$PREVIOUS_JSON" 2>/dev/null | grep '^objects/' || true)

# Remove tracked superseded objects (from git and disk)
for object in $TRACKED_OBJECTS; do
    basename=$(basename "$object")
    # Check if this object is in the deleted paths
    if echo "$DELETED_PATHS" | grep -q "$basename"; then
        # Also make sure it's not one of the active roots
        if [[ "$basename" != $(basename "$CURRENT_ROOT") ]] && \
           [[ "$basename" != $(basename "$PREVIOUS_ROOT") ]]; then
            echo "Removing superseded object: $object"
            git rm "$object" 2>/dev/null || true
        fi
    fi
done

# Also delete untracked superseded objects from disk to prevent accumulation
for deleted_path in $DELETED_PATHS; do
    object_path="$BEAD_DIR/$deleted_path"
    basename=$(basename "$deleted_path")
    # Skip if it's one of the active roots
    if [[ "$basename" == $(basename "$CURRENT_ROOT") ]] || \
       [[ "$basename" == $(basename "$PREVIOUS_ROOT") ]]; then
        continue
    fi
    # Delete from disk if exists
    if [[ -f "$object_path" ]]; then
        echo "Deleting untracked superseded object from disk: $deleted_path"
        rm "$object_path"
    fi
done

# Commit if changes were made
if git diff --cached --quiet; then
    echo "No checkpoint changes to commit"
    exit 0
fi

COMMIT_MESSAGE="${1:-chore: checkpoint commit with active root objects}"

git commit -m "$COMMIT_MESSAGE"

echo "✓ Checkpoint committed successfully"
echo "  Current: $CURRENT_ROOT"
echo "  Previous: $PREVIOUS_ROOT"
