#!/usr/bin/env bash
# Commit checkpoint changes with dynamic active_root resolution
#
# Usage: ./scripts/commit-checkpoint.sh "commit message"
#
# This script reads .beads/checkpoint/current.json and previous.json,
# extracts their active_root paths, and stages exactly those objects
# for commit alongside the pointer files.
#
# The active_root changes on every flush, so we resolve it dynamically
# at commit time rather than relying on static .gitignore patterns.

set -euo pipefail

# Check arguments
if [ $# -eq 0 ]; then
    echo "Usage: $0 \"commit message\"" >&2
    echo "Example: $0 \"chore: checkpoint commit\"" >&2
    exit 1
fi

COMMIT_MSG="$1"
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
    local path
    path=$(jq -r '.active_root.path // empty' "$file")

    if [ -z "$path" ]; then
        echo "Error: No active_root.path found in $file" >&2
        exit 1
    fi

    echo "$path"
}

# Extract active_root paths from both checkpoint files
CURRENT_ROOT=$(extract_active_root "current.json")
PREVIOUS_ROOT=$(extract_active_root "previous.json")

echo "Checkpoint commit script"
echo "========================"
echo "Current active_root: $CURRENT_ROOT"
echo "Previous active_root: $PREVIOUS_ROOT"

# Validate that active_root objects exist
validate_object_exists() {
    local path="$1"
    local full_path="$CHECKPOINT_DIR/$path"

    if [ ! -f "$full_path" ]; then
        echo "Error: Active root object not found: $full_path" >&2
        echo "Refusing to commit without all active root objects" >&2
        exit 1
    fi

    echo "✓ Found: $path"
}

echo ""
echo "Validating active root objects..."
validate_object_exists "$CURRENT_ROOT"
validate_object_exists "$PREVIOUS_ROOT"

# Stage files for commit
echo ""
echo "Staging files for commit..."

# Stage the three pointer files
echo "Staging pointer files..."
git add current.json previous.json forensic.jsonl

# Stage the active root objects
echo "Staging active root objects..."
git add "$CURRENT_ROOT" "$PREVIOUS_ROOT"

# Show what was staged
echo ""
echo "Staged files:"
git diff --cached --name-only

# Verify we have staged the expected files
EXPECTED_STAGED=5
ACTUAL_STAGED=$(git diff --cached --name-only | wc -l)
if [ "$ACTUAL_STAGED" -lt "$EXPECTED_STAGED" ]; then
    echo "Warning: Only $ACTUAL_STAGED files staged (expected at least $EXPECTED_STAGED)" >&2
    echo "This may indicate some files are already committed" >&2
fi

# Commit with the provided message
echo ""
echo "Committing with message: $COMMIT_MSG"
git commit -m "$COMMIT_MSG"

echo "✓ Checkpoint commit completed successfully"
