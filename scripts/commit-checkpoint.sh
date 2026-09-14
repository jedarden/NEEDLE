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
REPO_ROOT="$(git rev-parse --show-toplevel)"
CHECKPOINT_DIR="$REPO_ROOT/.beads/checkpoint"
# Resolve $0-relative paths BEFORE the `cd "$CHECKPOINT_DIR"` below. $0 is
# relative to the invoking shell's cwd, so after that cd it would resolve to
# .beads/checkpoint/scripts/ and the cleanup call near the end would fail
# with "No such file or directory" — aborting the whole run before staging.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

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

active_repo_path() {
    local path="$1"
    case "$path" in
        objects/*) printf '.beads/checkpoint/%s\n' "$path" ;;
        *)
            echo "Error: Active root is outside objects/: $path" >&2
            exit 1
            ;;
    esac
}

CURRENT_ROOT_REPO=$(active_repo_path "$CURRENT_ROOT")
PREVIOUS_ROOT_REPO=$(active_repo_path "$PREVIOUS_ROOT")

# Capture the pre-cleanup index view. A bead-rs flush can remove superseded
# objects from the worktree before this script runs, but they are still
# tracked until their deletions are added to the index.
TRACKED_OBJECTS_BEFORE=$(git -C "$REPO_ROOT" ls-files '.beads/checkpoint/objects/' || true)

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

# Cleanup superseded checkpoint objects
echo ""
echo "Cleaning up superseded checkpoint objects..."
"$SCRIPT_DIR/cleanup-superseded-checkpoint-objects.sh"

# Stage files for commit
echo ""
echo "Staging files for commit..."

# Stage the three pointer files
echo "Staging pointer files..."
git add current.json previous.json forensic.jsonl

# Stage the active root objects
echo "Staging active root objects..."
git add "$CURRENT_ROOT" "$PREVIOUS_ROOT"

# Stage tracked-object removals even when bead-rs already deleted the files
# from the worktree. `git rm` in the cleanup helper cannot remove a path that
# is already absent, whereas `git add -u` records that deletion in the index.
git -C "$REPO_ROOT" add -u -- '.beads/checkpoint/objects/'

verify_checkpoint_object_shape() {
    local superseded=""
    local object
    while IFS= read -r object; do
        [ -n "$object" ] || continue
        if [ "$object" != "$CURRENT_ROOT_REPO" ] && [ "$object" != "$PREVIOUS_ROOT_REPO" ]; then
            superseded="${superseded}${superseded:+$'\n'}${object}"
        fi
    done <<< "$TRACKED_OBJECTS_BEFORE"

    # Every previously tracked object outside the active pair must be staged
    # as deleted. Use --no-renames here so a rename source is still visible as
    # its underlying deletion.
    while IFS= read -r object; do
        [ -n "$object" ] || continue
        if ! git -C "$REPO_ROOT" diff --cached --no-renames --diff-filter=D \
            --name-only -- "$object" | grep -Fqx "$object"; then
            echo "Error: Superseded checkpoint object removal was not staged: $object" >&2
            exit 1
        fi
    done <<< "$superseded"

    # When superseded history exists, every newly added active root must pair
    # with one of those deletions under Git's normal rename detection. Without
    # that pairing Forgejo must scan the whole monolithic snapshot as new text.
    # A first checkpoint, or a transition with no superseded object, is a
    # legitimate addition and has nothing it could pair with.
    [ -n "$superseded" ] || return 0

    local rename_diff
    rename_diff=$(git -C "$REPO_ROOT" diff --cached --name-status -M -- \
        '.beads/checkpoint/objects/')
    for object in "$CURRENT_ROOT_REPO" "$PREVIOUS_ROOT_REPO"; do
        if git -C "$REPO_ROOT" diff --cached --no-renames --diff-filter=A \
            --name-only -- "$object" | grep -Fqx "$object"; then
            if ! awk -F '\t' -v destination="$object" \
                '$1 ~ /^R[0-9]+$/ && $3 == destination { found = 1 } END { exit !found }' \
                <<< "$rename_diff"; then
                echo "Error: New checkpoint root is not paired with a superseded-object rename: $object" >&2
                echo "Refusing a whole-object addition while tracked superseded history exists." >&2
                exit 1
            fi
        fi
    done
}

verify_checkpoint_object_shape

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
