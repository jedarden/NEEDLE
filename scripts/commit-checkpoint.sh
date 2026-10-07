#!/usr/bin/env bash
# Publish bead-rs checkpoint state without sweeping concurrent work into the commit.
# Usage: scripts/commit-checkpoint.sh [--force] [--workspace PATH] "commit message"

set -euo pipefail

FORCE=false
WORKSPACE=""
POSITIONAL=()
while (($# > 0)); do
    case "$1" in
        --force)
            FORCE=true
            shift
            ;;
        --workspace)
            (($# >= 2)) || { echo "Error: --workspace requires a path" >&2; exit 2; }
            WORKSPACE="$2"
            shift 2
            ;;
        --)
            shift
            POSITIONAL+=("$@")
            break
            ;;
        *)
            POSITIONAL+=("$1")
            shift
            ;;
    esac
done
[[ ${#POSITIONAL[@]} -eq 1 ]] || {
    echo "Usage: $0 [--force] [--workspace PATH] \"commit message\"" >&2
    exit 2
}
COMMIT_MSG="${POSITIONAL[0]}"
REPO_LOOKUP="${WORKSPACE:-$PWD}"
REPO_ROOT="$(git -C "$REPO_LOOKUP" rev-parse --show-toplevel 2>/dev/null)" || {
    echo "Error: Not in a Git worktree" >&2
    exit 1
}
CHECKPOINT_DIR="$REPO_ROOT/.beads/checkpoint"
CHECKPOINT_RELATIVE=".beads/checkpoint"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
[[ -d "$CHECKPOINT_DIR" ]] || {
    echo "Error: Checkpoint directory not found: $CHECKPOINT_DIR" >&2
    exit 1
}

# bead-rs uses this advisory lock while replacing checkpoint files. Hold it
# across validation, pruning, staging, and commit so a concurrent flush cannot
# make the published pointers disagree with the roots we validated.
if [[ "${NEEDLE_CHECKPOINT_PUBLISH_LOCK_HELD:-0}" != 1 ]]; then
    command -v flock >/dev/null 2>&1 || {
        echo "Error: flock is required to coordinate checkpoint publication" >&2
        exit 1
    }
    lock_args=()
    [[ "$FORCE" != true ]] || lock_args+=(--force)
    [[ -z "$WORKSPACE" ]] || lock_args+=(--workspace "$WORKSPACE")
    lock_args+=("$COMMIT_MSG")
    exec flock -x "$CHECKPOINT_DIR/publish.lock" env NEEDLE_CHECKPOINT_PUBLISH_LOCK_HELD=1 "$0" "${lock_args[@]}"
fi

cd "$REPO_ROOT"

DEBOUNCE_SECONDS="${NEEDLE_CHECKPOINT_COMMIT_DEBOUNCE_SECONDS:-900}"
[[ "$DEBOUNCE_SECONDS" =~ ^[0-9]+$ ]] || {
    echo "Error: NEEDLE_CHECKPOINT_COMMIT_DEBOUNCE_SECONDS must be a non-negative integer" >&2
    exit 2
}
DEBOUNCE_STATE_DIR="${NEEDLE_HOME:-$HOME/.needle}/checkpoint-commit-state"
DEBOUNCE_MARKER="$DEBOUNCE_STATE_DIR/$(printf '%s' "$REPO_ROOT" | cksum | awk '{print $1}')"

CHECKPOINT_STATUS="$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all -- "$CHECKPOINT_RELATIVE")"
if [[ -z "$CHECKPOINT_STATUS" ]]; then
    echo "CHECKPOINT_COMMIT_NOOP: checkpoint is already committed"
    exit 0
fi

validate_root() {
    local pointer="$1" value path digest file actual
    value="$(jq -er '
        .active_root as $root
        | select(($root.path | type) == "string")
        | select($root.path | test("^objects/[0-9a-f]+\\.jsonl$"))
        | select(($root.sha256 | type) == "string")
        | select($root.sha256 | test("^[0-9a-f]{64}$"))
        | [$root.path, $root.sha256] | @tsv
    ' "$CHECKPOINT_DIR/$pointer")" || {
        echo "Error: Invalid $pointer active root" >&2
        return 1
    }
    IFS=$'\t' read -r path digest <<< "$value"
    file="$CHECKPOINT_DIR/$path"
    [[ -f "$file" ]] || {
        echo "Error: $pointer references missing root: $path" >&2
        return 1
    }
    actual="$(sha256sum "$file" | awk '{print $1}')"
    [[ "$actual" == "$digest" ]] || {
        echo "Error: $pointer root hash mismatch for $path" >&2
        return 1
    }
    printf '%s\n' "$path"
}

CURRENT_ROOT="$(validate_root current.json)" || exit 1
PREVIOUS_ROOT="$(validate_root previous.json)" || exit 1
[[ -f "$CHECKPOINT_DIR/forensic.jsonl" ]] || {
    echo "Error: Missing checkpoint view: $CHECKPOINT_DIR/forensic.jsonl" >&2
    exit 1
}

CURRENT_ROOT_REPO="$CHECKPOINT_RELATIVE/$CURRENT_ROOT"
PREVIOUS_ROOT_REPO="$CHECKPOINT_RELATIVE/$PREVIOUS_ROOT"

# Keep the exact path set for this publication. --only below commits these
# checkpoint paths and leaves every other staged or unstaged edit untouched.
COMMIT_PATHS=(
    "$CHECKPOINT_RELATIVE/current.json"
    "$CHECKPOINT_RELATIVE/previous.json"
    "$CHECKPOINT_RELATIVE/forensic.jsonl"
    "$CURRENT_ROOT_REPO"
    "$PREVIOUS_ROOT_REPO"
)
TRACKED_OBJECTS_BEFORE=()
while IFS= read -r -d '' object; do
    TRACKED_OBJECTS_BEFORE+=("$object")
    if [[ "$object" != "$CURRENT_ROOT_REPO" && "$object" != "$PREVIOUS_ROOT_REPO" ]]; then
        COMMIT_PATHS+=("$object")
    fi
done < <(git -C "$REPO_ROOT" ls-files -z -- "$CHECKPOINT_RELATIVE/objects/")

TARGET_STATUS="$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all -- "${COMMIT_PATHS[@]}")"
if [[ -z "$TARGET_STATUS" ]]; then
    echo "CHECKPOINT_COMMIT_NOOP: no publishable checkpoint changes"
    exit 0
fi

if [[ "$FORCE" != true && -f "$DEBOUNCE_MARKER" ]]; then
    LAST_COMMIT_EPOCH="$(cat "$DEBOUNCE_MARKER" 2>/dev/null || echo 0)"
    NOW_EPOCH="$(date +%s)"
    if [[ "$LAST_COMMIT_EPOCH" =~ ^[0-9]+$ ]]; then
        ELAPSED=$((NOW_EPOCH - LAST_COMMIT_EPOCH))
        if ((ELAPSED >= 0 && ELAPSED < DEBOUNCE_SECONDS)); then
            echo "CHECKPOINT_COMMIT_SKIPPED_DEBOUNCE: last standalone checkpoint commit for this repo was ${ELAPSED}s ago (< ${DEBOUNCE_SECONDS}s)."
            echo "Checkpoint changes remain durable and uncommitted for the next publication."
            exit 0
        fi
    fi
fi

# This validates both pointer schemas, root hashes, and the forensic view
# before pruning stale objects. It stages roots, pointers, and stale tracked
# deletions with one atomic Git index update.
"$SCRIPT_DIR/checkpoint-publish.sh" stage

for object in "${TRACKED_OBJECTS_BEFORE[@]}"; do
    if [[ "$object" == "$CURRENT_ROOT_REPO" || "$object" == "$PREVIOUS_ROOT_REPO" ]]; then
        continue
    fi
    if ! git -C "$REPO_ROOT" diff --cached --no-renames --diff-filter=D --name-only -- "$object" | grep -Fqx "$object"; then
        echo "Error: Superseded checkpoint object removal was not staged: $object" >&2
        exit 1
    fi
done

# Git may display similar roots as renames. Similarity is presentation, not
# checkpoint integrity: a large valid change must still be publishable.

# Phase 20 (needle-9e778fad): file-contention markers are checkout-local runtime
# state and are never published, whatever ends up in the path list.
for path in "${COMMIT_PATHS[@]}"; do
    case "$path" in
        .needle/locks|.needle/locks/*)
            echo "Error: refusing to commit file-contention marker: $path" >&2
            exit 1
            ;;
    esac
done

git -C "$REPO_ROOT" -c user.email=github@jedarden.com -c user.name=jedarden \
    commit --only -m "$COMMIT_MSG" -- "${COMMIT_PATHS[@]}"

mkdir -p "$DEBOUNCE_STATE_DIR" 2>/dev/null || true
date +%s > "$DEBOUNCE_MARKER" 2>/dev/null || true
echo "Checkpoint publication committed; unrelated staged and unstaged changes remain untouched."
