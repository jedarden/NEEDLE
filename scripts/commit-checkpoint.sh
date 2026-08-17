#!/usr/bin/env bash
# Commit checkpoint with all required object files
# This script ensures that every checkpoint commit includes the active root
# objects referenced by current.json and previous.json, while excluding
# superseded objects listed in deleted_paths.

set -euo pipefail

CHECKPOINT_DIR=".beads/checkpoint"
CURRENT_JSON="$CHECKPOINT_DIR/current.json"
PREVIOUS_JSON="$CHECKPOINT_DIR/previous.json"

# Extract active root paths from the pointers
extract_active_root() {
    local json_file="$1"
    if [[ -f "$json_file" ]]; then
        jq -r '.active_root.path // empty' "$json_file"
    fi
}

# Get the active root objects
CURRENT_ROOT=$(extract_active_root "$CURRENT_JSON")
PREVIOUS_ROOT=$(extract_active_root "$PREVIOUS_JSON")

echo "Checkpoint commit script"
echo "========================"

if [[ -n "$CURRENT_ROOT" ]]; then
    echo "Current active root: $CURRENT_ROOT"
else
    echo "Warning: Could not extract current root from $CURRENT_JSON"
fi

if [[ -n "$PREVIOUS_ROOT" ]]; then
    echo "Previous active root: $PREVIOUS_ROOT"
else
    echo "Warning: Could not extract previous root from $PREVIOUS_JSON"
fi

# Always commit the three pointer files
POINTER_FILES=(
    "$CHECKPOINT_DIR/current.json"
    "$CHECKPOINT_DIR/previous.json"
    "$CHECKPOINT_DIR/forensic.jsonl"
)

# Add active root objects if they exist and are not already in git
ACTIVE_ROOTS=()
for root in "$CURRENT_ROOT" "$PREVIOUS_ROOT"; do
    if [[ -n "$root" && -f "$CHECKPOINT_DIR/$root" ]]; then
        ACTIVE_ROOTS+=("$CHECKPOINT_DIR/$root")
        echo "Will include active root: $root"
    elif [[ -n "$root" ]]; then
        echo "Warning: Active root $root does not exist, skipping"
    fi
done

# Check if there's anything to commit
if [[ ${#ACTIVE_ROOTS[@]} -eq 0 ]] && ! git diff --quiet "${POINTER_FILES[@]}"; then
    echo "Error: Pointer files modified but no active root objects found"
    echo "This would create a broken checkpoint state"
    exit 1
fi

# Stage the files
echo ""
echo "Staging files..."
for file in "${POINTER_FILES[@]}" "${ACTIVE_ROOTS[@]}"; do
    if [[ -f "$file" ]]; then
        echo "  + $file"
        git add "$file"
    fi
done

echo ""
echo "Files staged. Please commit with:"
echo "  git commit -m 'chore: checkpoint flush'"
