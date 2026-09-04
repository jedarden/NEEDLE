#!/usr/bin/env bash
# Publish the bead-rs checkpoint without leaving generation objects behind.
#
# Usage:
#   scripts/checkpoint-publish.sh stage
#   scripts/checkpoint-publish.sh commit -m "chore(beads): checkpoint"
#   scripts/checkpoint-publish.sh verify-index
#
# The active generation names are deliberately read from current.json and
# previous.json.  Generation IDs change on every flush, so they must never be
# listed statically in a Git command or ignore rule.

set -euo pipefail

readonly CHECKPOINT_DIR=".beads/checkpoint"
readonly CURRENT_POINTER="$CHECKPOINT_DIR/current.json"
readonly PREVIOUS_POINTER="$CHECKPOINT_DIR/previous.json"

die() {
    printf 'checkpoint-publish: error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat >&2 <<'EOF'
Usage:
  scripts/checkpoint-publish.sh stage
  scripts/checkpoint-publish.sh commit -m "commit message"
  scripts/checkpoint-publish.sh verify-index

stage parses current.json and previous.json, validates both referenced roots,
stages those roots with the checkpoint pointers/view, and removes superseded
generation objects from the working tree.
EOF
    exit 2
}

require_commands() {
    command -v git >/dev/null 2>&1 || die "git is required"
    command -v python3 >/dev/null 2>&1 || die "python3 is required to parse checkpoint pointers"
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required to verify checkpoint roots"
}

repo_root() {
    git rev-parse --show-toplevel 2>/dev/null || die "not inside a Git worktree"
}

# Print "path<TAB>sha256" after validating the pointer schema and path safety.
parse_pointer_json() {
    python3 -c '
import json
import re
import sys

try:
    document = json.load(sys.stdin)
    root = document["active_root"]
    path = root["path"]
    digest = root["sha256"]
except (KeyError, TypeError, json.JSONDecodeError) as error:
    raise SystemExit(f"invalid checkpoint pointer: {error}")

if not isinstance(path, str) or not re.fullmatch(r"objects/[0-9a-f]+\.jsonl", path):
    raise SystemExit(f"invalid active_root path: {path!r}")
if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
    raise SystemExit(f"invalid active_root sha256: {digest!r}")

print(f"{path}\t{digest}")
'
}

parse_pointer_file() {
    local pointer_file="$1"
    [[ -f "$pointer_file" ]] || die "missing pointer: $pointer_file"
    parse_pointer_json <"$pointer_file" || die "cannot parse pointer: $pointer_file"
}

parse_index_pointer() {
    local pointer_file="$1"
    local contents

    contents="$(git show ":$pointer_file" 2>/dev/null)" \
        || die "staged checkpoint is missing $pointer_file"
    printf '%s' "$contents" | parse_pointer_json \
        || die "cannot parse staged pointer: $pointer_file"
}

pointer_value() {
    local pointer_file="$1"
    local value
    value="$(parse_pointer_file "$pointer_file")" \
        || die "cannot read pointer: $pointer_file"
    [[ "$value" == *$'\t'* ]] || die "pointer did not contain an active root: $pointer_file"
    printf '%s\n' "$value"
}

verify_working_root() {
    local pointer_name="$1"
    local value="$2"
    local root_path="${value%%$'\t'*}"
    local expected_sha="${value#*$'\t'}"
    local root_file="$CHECKPOINT_DIR/$root_path"
    local actual_sha

    [[ -f "$root_file" ]] || die "$pointer_name references missing root: $root_path"
    actual_sha="$(sha256sum "$root_file" | awk '{print $1}')"
    [[ "$actual_sha" == "$expected_sha" ]] \
        || die "$pointer_name root hash mismatch for $root_path (expected $expected_sha, got $actual_sha)"
}

verify_index_root() {
    local pointer_name="$1"
    local value="$2"
    local root_path="${value%%$'\t'*}"
    local expected_sha="${value#*$'\t'}"
    local index_root="$CHECKPOINT_DIR/$root_path"
    local actual_sha
    local object_type

    object_type="$(git cat-file -t ":$index_root" 2>/dev/null)" \
        || die "staged $pointer_name root is missing from the index: $root_path"
    [[ "$object_type" == "blob" ]] \
        || die "staged $pointer_name root is not a regular file: $root_path"
    actual_sha="$(git show ":$index_root" | sha256sum | awk '{print $1}')"
    [[ "$actual_sha" == "$expected_sha" ]] \
        || die "staged $pointer_name root hash mismatch for $root_path (expected $expected_sha, got $actual_sha)"
}

verify_index() {
    local staged_paths
    staged_paths="$(git diff --cached --name-only -- "$CHECKPOINT_DIR")"

    if ! grep -Fxq "$CURRENT_POINTER" <<<"$staged_paths" \
        && ! grep -Fxq "$PREVIOUS_POINTER" <<<"$staged_paths"; then
        return 0
    fi

    local current_value previous_value
    current_value="$(parse_index_pointer "$CURRENT_POINTER")"
    previous_value="$(parse_index_pointer "$PREVIOUS_POINTER")"
    verify_index_root "current.json" "$current_value"
    verify_index_root "previous.json" "$previous_value"
}

stage_checkpoint() {
    local current_value previous_value
    current_value="$(pointer_value "$CURRENT_POINTER")"
    previous_value="$(pointer_value "$PREVIOUS_POINTER")"
    verify_working_root "current.json" "$current_value"
    verify_working_root "previous.json" "$previous_value"

    local current_root="$CHECKPOINT_DIR/${current_value%%$'\t'*}"
    local previous_root="$CHECKPOINT_DIR/${previous_value%%$'\t'*}"
    local objects_dir="$CHECKPOINT_DIR/objects"
    [[ -f "$CHECKPOINT_DIR/forensic.jsonl" ]] \
        || die "missing checkpoint view: $CHECKPOINT_DIR/forensic.jsonl"

    local stale_count=0
    local object_file object_name object_path
    local -a tracked_stale=()
    while IFS= read -r -d '' object_file; do
        object_name="${object_file##*/}"
        object_path="$objects_dir/$object_name"
        if [[ "$object_file" == "$current_root" || "$object_file" == "$previous_root" ]]; then
            continue
        fi

        if git ls-files --error-unmatch -- "$object_path" >/dev/null 2>&1; then
            tracked_stale+=("$object_path")
        fi
        rm -f -- "$object_file"
        stale_count=$((stale_count + 1))
    done < <(find "$objects_dir" -maxdepth 1 -type f -name '*.jsonl' -print0)

    local -a checkpoint_paths=(
        "$CURRENT_POINTER"
        "$PREVIOUS_POINTER"
        "$CHECKPOINT_DIR/forensic.jsonl"
        "$current_root"
        "$previous_root"
    )
    git add -- "${checkpoint_paths[@]}"
    if ((${#tracked_stale[@]} > 0)); then
        git add -u -- "${tracked_stale[@]}"
    fi

    verify_index
    printf 'checkpoint-publish: staged current=%s previous=%s; pruned %d superseded object(s)\n' \
        "${current_value%%$'\t'*}" "${previous_value%%$'\t'*}" "$stale_count"
}

main() {
    require_commands
    local root
    root="$(repo_root)"
    cd "$root"

    local command="${1:-}"
    case "$command" in
        stage)
            [[ $# -eq 1 ]] || usage
            stage_checkpoint
            ;;
        verify-index)
            [[ $# -eq 1 ]] || usage
            verify_index
            ;;
        commit)
            shift
            [[ $# -gt 0 ]] || usage
            stage_checkpoint
            git commit "$@"
            ;;
        *)
            usage
            ;;
    esac
}

main "$@"
