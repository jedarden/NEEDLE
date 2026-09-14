#!/usr/bin/env bash
# Focused regression for scripts/commit-checkpoint.sh.
#
# Every case runs a copied script in a disposable Git repository. This test
# must never invoke the checkpoint commit helper against the NEEDLE checkout.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
SCRIPT_UNDER_TEST="$REPO_ROOT/scripts/commit-checkpoint.sh"
CLEANUP_HELPER="$REPO_ROOT/scripts/cleanup-superseded-checkpoint-objects.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-checkpoint-shape.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

new_repo() {
    local name="$1"
    local repo="$TEST_ROOT/$name"
    mkdir -p "$repo/scripts" "$repo/.beads/checkpoint/objects"
    cp "$SCRIPT_UNDER_TEST" "$repo/scripts/commit-checkpoint.sh"
    cp "$CLEANUP_HELPER" "$repo/scripts/cleanup-superseded-checkpoint-objects.sh"
    chmod +x "$repo/scripts/"*.sh
    git -C "$repo" init -q
    git -C "$repo" config user.name "checkpoint shape test"
    git -C "$repo" config user.email "checkpoint-shape@example.invalid"
    git -C "$repo" add scripts
    git -C "$repo" commit -qm "test fixture scripts"
    printf '%s\n' "$repo"
}

write_similar_object() {
    local path="$1"
    local family="$2"
    local generation="$3"
    {
        printf '{"generation":"%s"}\n' "$generation"
        local record
        for record in $(seq 1 200); do
            printf '{"family":"%s","record":%s,"value":"stable"}\n' \
                "$family" "$record"
        done
    } > "$path"
}

write_checkpoint() {
    local repo="$1"
    local current_name="$2"
    local previous_name="$3"
    local generation="$4"
    local checkpoint="$repo/.beads/checkpoint"

    # bead-rs replaces the active pair during flush; critically, the old
    # tracked objects are already absent before commit-checkpoint.sh runs.
    rm -f "$checkpoint/objects/"*.jsonl
    write_similar_object "$checkpoint/objects/$current_name" current "$generation"
    write_similar_object "$checkpoint/objects/$previous_name" previous "$generation"
    printf '{"active_root":{"path":"objects/%s"}}\n' "$current_name" \
        > "$checkpoint/current.json"
    printf '{"active_root":{"path":"objects/%s"}}\n' "$previous_name" \
        > "$checkpoint/previous.json"
    printf '{"generation":"%s"}\n' "$generation" > "$checkpoint/forensic.jsonl"
}

run_checkpoint_commit() {
    local repo="$1"
    local message="$2"
    (cd "$repo" && ./scripts/commit-checkpoint.sh "$message")
}

assert_rename_only_object_diff() {
    local repo="$1"
    local shape
    shape=$(git -C "$repo" show --format= --name-status -M -- \
        .beads/checkpoint/objects)
    local rename_count
    rename_count=$(awk -F '\t' '$1 ~ /^R[0-9]+$/ { count++ } END { print count + 0 }' \
        <<< "$shape")
    [ "$rename_count" -eq 2 ] || fail "expected two checkpoint-object renames, got: $shape"
    if awk -F '\t' '$1 == "A" || $1 == "D" { found = 1 } END { exit !found }' \
        <<< "$shape"; then
        fail "checkpoint objects were added/deleted instead of rename-paired: $shape"
    fi

    local stat
    stat=$(git -C "$repo" show --format= --stat -M -- .beads/checkpoint/objects)
    grep -Fq '=>' <<< "$stat" || fail "--stat -M did not report rename shape: $stat"
}

test_first_checkpoint_and_successive_flushes() {
    local repo
    repo=$(new_repo successive)

    write_checkpoint "$repo" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo" "checkpoint first" >/dev/null
    local first_shape
    first_shape=$(git -C "$repo" show --format= --name-status -- \
        .beads/checkpoint/objects)
    [ "$(grep -c '^A' <<< "$first_shape")" -eq 2 ] \
        || fail "first checkpoint did not add both roots: $first_shape"

    write_checkpoint "$repo" current-b.jsonl previous-b.jsonl second
    run_checkpoint_commit "$repo" "checkpoint second" >/dev/null
    assert_rename_only_object_diff "$repo"

    write_checkpoint "$repo" current-c.jsonl previous-c.jsonl third
    run_checkpoint_commit "$repo" "checkpoint third" >/dev/null
    assert_rename_only_object_diff "$repo"

    echo "PASS: first checkpoint and two worktree-deleted superseder flushes"
}

test_unpaired_addition_fails_closed() {
    local repo
    repo=$(new_repo unpaired)
    write_checkpoint "$repo" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo" "checkpoint first" >/dev/null
    local before
    before=$(git -C "$repo" rev-parse HEAD)

    local checkpoint="$repo/.beads/checkpoint"
    rm -f "$checkpoint/objects/"*.jsonl
    printf '%s\n' "unrelated-current-payload" > "$checkpoint/objects/current-z.jsonl"
    printf '%s\n' "unrelated-previous-payload" > "$checkpoint/objects/previous-z.jsonl"
    printf '%s\n' '{"active_root":{"path":"objects/current-z.jsonl"}}' \
        > "$checkpoint/current.json"
    printf '%s\n' '{"active_root":{"path":"objects/previous-z.jsonl"}}' \
        > "$checkpoint/previous.json"
    printf '%s\n' '{"generation":"unpaired"}' > "$checkpoint/forensic.jsonl"

    local output="$TEST_ROOT/unpaired-output.log"
    if run_checkpoint_commit "$repo" "checkpoint unpaired" >"$output" 2>&1; then
        fail "unpaired whole-object additions unexpectedly committed"
    fi
    grep -Fq "New checkpoint root is not paired with a superseded-object rename" "$output" \
        || fail "unpaired failure did not explain the rename-shape requirement"
    [ "$(git -C "$repo" rev-parse HEAD)" = "$before" ] \
        || fail "unpaired failure created a commit"

    echo "PASS: unpaired additions with tracked superseded roots fail closed"
}

test_first_checkpoint_and_successive_flushes
test_unpaired_addition_fails_closed
echo "All checkpoint rename-shape tests passed"
