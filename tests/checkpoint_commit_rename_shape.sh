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

# The debounce marker (needle-b27f84fe) lives under NEEDLE_HOME, not the
# repo being committed. Isolate it into the disposable test root so this
# test can never read or write the real ~/.needle/checkpoint-commit-state/.
export NEEDLE_HOME="$TEST_ROOT/.needle-home"

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
    # --force: this helper is used by the rename-shape cases below, which
    # need every call to actually produce a commit regardless of the
    # debounce window. Debounce/force/piggyback behavior itself is covered
    # by the dedicated test_* functions further down, which call the script
    # directly so they can observe an un-forced, debounced invocation.
    (cd "$repo" && ./scripts/commit-checkpoint.sh --force "$message")
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

test_standalone_commit_is_debounced() {
    local repo
    repo=$(new_repo debounced)

    write_checkpoint "$repo" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo" "checkpoint first" >/dev/null
    local before
    before=$(git -C "$repo" rev-parse HEAD)

    write_checkpoint "$repo" current-b.jsonl previous-b.jsonl second
    local output="$TEST_ROOT/debounced-output.log"
    (cd "$repo" && ./scripts/commit-checkpoint.sh "checkpoint second") >"$output" 2>&1 \
        || fail "a debounced skip must exit 0, not fail the caller"
    grep -Fq "CHECKPOINT_COMMIT_SKIPPED_DEBOUNCE" "$output" \
        || fail "debounced call did not report a skip: $(cat "$output")"
    [ "$(git -C "$repo" rev-parse HEAD)" = "$before" ] \
        || fail "a debounced call must not create a commit"
    git -C "$repo" status --porcelain | grep -q '\.beads/checkpoint/' \
        || fail "debounced checkpoint changes must remain uncommitted in the working tree, not be lost"

    echo "PASS: a second standalone checkpoint commit within the debounce window is skipped, not lost"
}

test_force_overrides_debounce() {
    local repo
    repo=$(new_repo forced)

    write_checkpoint "$repo" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo" "checkpoint first" >/dev/null
    local before
    before=$(git -C "$repo" rev-parse HEAD)

    write_checkpoint "$repo" current-b.jsonl previous-b.jsonl second
    (cd "$repo" && ./scripts/commit-checkpoint.sh --force "checkpoint second") >/dev/null

    [ "$(git -C "$repo" rev-parse HEAD)" != "$before" ] \
        || fail "--force must commit immediately even inside the debounce window"

    echo "PASS: --force commits a standalone checkpoint immediately regardless of debounce"
}

test_other_staged_work_bypasses_debounce() {
    local repo
    repo=$(new_repo piggyback)

    write_checkpoint "$repo" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo" "checkpoint first" >/dev/null
    local before
    before=$(git -C "$repo" rev-parse HEAD)

    write_checkpoint "$repo" current-b.jsonl previous-b.jsonl second
    printf 'real change\n' > "$repo/README.md"
    git -C "$repo" add README.md

    (cd "$repo" && ./scripts/commit-checkpoint.sh "checkpoint second") >/dev/null

    [ "$(git -C "$repo" rev-parse HEAD)" != "$before" ] \
        || fail "a checkpoint commit with other real work already staged must never be debounced"
    git -C "$repo" show --format= --name-only HEAD | grep -Fqx "README.md" \
        || fail "the already-staged non-checkpoint file was not included in the piggybacked commit"

    echo "PASS: other staged work rides the checkpoint along instead of being debounced"
}

test_debounce_is_per_repo() {
    local repo_a repo_b
    repo_a=$(new_repo scope-a)
    repo_b=$(new_repo scope-b)

    write_checkpoint "$repo_a" current-a.jsonl previous-a.jsonl first
    run_checkpoint_commit "$repo_a" "checkpoint first" >/dev/null

    write_checkpoint "$repo_b" current-a.jsonl previous-a.jsonl first
    local before_b
    before_b=$(git -C "$repo_b" rev-parse HEAD 2>/dev/null || echo none)
    (cd "$repo_b" && ./scripts/commit-checkpoint.sh "checkpoint first") >/dev/null

    [ "$(git -C "$repo_b" rev-parse HEAD)" != "$before_b" ] \
        || fail "a fresh repo's first checkpoint commit must not be debounced by an unrelated repo's marker"

    echo "PASS: the debounce marker is scoped per repository, not shared"
}

test_first_checkpoint_and_successive_flushes
test_unpaired_addition_fails_closed
test_standalone_commit_is_debounced
test_force_overrides_debounce
test_other_staged_work_bypasses_debounce
test_debounce_is_per_repo
echo "All checkpoint rename-shape tests passed"
