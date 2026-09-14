#!/usr/bin/env bash
# Contract tests for exact-index pre-commit verification and evidence handoff.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/needle-precommit-test.XXXXXX")"
trap 'find "$tmp_root" -depth -delete' EXIT

passes=0
failures=0

pass() {
    printf 'PASS: %s\n' "$1"
    passes=$((passes + 1))
}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

make_fixture() {
    local name="$1"
    local fixture="$tmp_root/$name"

    mkdir -p "$fixture/.githooks" "$fixture/scripts"
    cp "$repo_root/.githooks/pre-commit" "$fixture/.githooks/"
    cp "$repo_root/scripts/bypass-detection.sh" "$fixture/scripts/"
    cat > "$fixture/scripts/definition-of-done.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bypass-detection.sh"

printf '%s\n' "$PWD" > "$TEST_RECORD_DIR/materialized-root"
printf '%s\n' "${CARGO_TARGET_DIR:-}" > "$TEST_RECORD_DIR/cargo-target-dir"
printf '%s\n' "$*" > "$TEST_RECORD_DIR/dod-arguments"

[[ "$(cat gate.txt)" != broken ]] || {
    printf 'staged gate is broken\n' >&2
    exit 1
}
[[ "$(cat sibling.txt)" == safe ]] || {
    printf 'unstaged sibling contaminated the gate\n' >&2
    exit 1
}

if [[ "${TEST_MUTATE_INDEX:-}" == 1 ]]; then
    printf 'mutated during verification\n' > "$TEST_REAL_ROOT/index-mutation.txt"
    env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
        git -C "$TEST_REAL_ROOT" add index-mutation.txt
fi

needle_mark_verified fast
EOF
    cat > "$fixture/scripts/secret-scan.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == staged ]]
EOF
    cat > "$fixture/scripts/checkpoint-publish.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == verify-index ]]
EOF
    chmod +x "$fixture/.githooks/pre-commit" "$fixture/scripts/"*.sh

    printf 'base\n' > "$fixture/gate.txt"
    printf 'safe\n' > "$fixture/sibling.txt"
    git -C "$fixture" init -q
    git -C "$fixture" config user.name test-user
    git -C "$fixture" config user.email test@example.invalid
    git -C "$fixture" add .githooks scripts gate.txt sibling.txt
    git -C "$fixture" commit --quiet --no-verify -m initial
    git -C "$fixture" config core.hooksPath .githooks
    printf '%s\n' "$fixture"
}

state_path() {
    local fixture="$1" parent="$2" tree="$3"
    printf '%s/needle-verification-state/%s-%s\n' \
        "$(git -C "$fixture" rev-parse --absolute-git-dir)" "$parent" "$tree"
}

run_hook() {
    local fixture="$1" record_dir="$2"
    shift 2
    mkdir -p "$record_dir"
    (
        cd "$fixture"
        TEST_REAL_ROOT="$fixture" TEST_RECORD_DIR="$record_dir" "$@" \
            .githooks/pre-commit
    )
}

# A broken staged file must fail even when the working tree contains an
# unstaged fix for the same path.
fixture="$(make_fixture staged-broken)"
record_dir="$tmp_root/staged-broken-record"
printf 'broken\n' > "$fixture/gate.txt"
git -C "$fixture" add gate.txt
captured_parent="$(git -C "$fixture" rev-parse HEAD)"
captured_tree="$(git -C "$fixture" write-tree)"
printf 'fixed only in working tree\n' > "$fixture/gate.txt"
if run_hook "$fixture" "$record_dir" >"$record_dir.output" 2>&1; then
    fail 'broken staged tree cannot borrow an unstaged fix'
elif grep -Fq 'staged gate is broken' "$record_dir.output" \
    && [[ ! -e "$(state_path "$fixture" "$captured_parent" "$captured_tree")" ]]; then
    pass 'broken staged tree cannot borrow an unstaged fix'
else
    fail 'broken staged tree failed without the staged-tree verdict'
fi

# A clean staged candidate must ignore an unstaged break in a sibling file.
fixture="$(make_fixture sibling-broken)"
record_dir="$tmp_root/sibling-broken-record"
printf 'candidate\n' > "$fixture/gate.txt"
git -C "$fixture" add gate.txt
captured_parent="$(git -C "$fixture" rev-parse HEAD)"
captured_tree="$(git -C "$fixture" write-tree)"
printf 'broken\n' > "$fixture/sibling.txt"
if run_hook "$fixture" "$record_dir" >"$record_dir.output" 2>&1; then
    expected_state="$(state_path "$fixture" "$captured_parent" "$captured_tree")"
    materialized_root="$(cat "$record_dir/materialized-root")"
    cargo_target_dir="$(cat "$record_dir/cargo-target-dir")"
    expected_target_dir="${CARGO_TARGET_DIR:-$fixture/target}"
    if [[ "$expected_target_dir" != /* ]]; then
        expected_target_dir="$fixture/$expected_target_dir"
    fi
    if [[ -f "$expected_state" ]] \
        && grep -Fxq 'status=verified' "$expected_state" \
        && [[ ! -e "$materialized_root" ]] \
        && [[ "$cargo_target_dir" == "$expected_target_dir" ]] \
        && grep -Fxq -- '--fast --count-bypass --changed-only' "$record_dir/dod-arguments"; then
        pass 'unstaged sibling break cannot contaminate a clean staged tree'
        pass 'successful handoff names only the tested parent and tree'
        pass 'temporary tree is cleaned and uses the reusable build cache'
    else
        fail 'successful exact-tree verification produced incorrect evidence or cleanup'
    fi
else
    fail 'unstaged sibling break contaminated the clean staged tree'
fi

# A process that stages another path while the checks run changes the candidate
# identity. The hook must fail and leave no marker for either tree.
fixture="$(make_fixture index-mutated)"
record_dir="$tmp_root/index-mutated-record"
printf 'candidate\n' > "$fixture/gate.txt"
git -C "$fixture" add gate.txt
captured_parent="$(git -C "$fixture" rev-parse HEAD)"
captured_tree="$(git -C "$fixture" write-tree)"
captured_state="$(state_path "$fixture" "$captured_parent" "$captured_tree")"
if run_hook "$fixture" "$record_dir" env TEST_MUTATE_INDEX=1 \
    >"$record_dir.output" 2>&1; then
    fail 'index mutation during verification was accepted'
else
    mutated_tree="$(git -C "$fixture" write-tree)"
    mutated_state="$(state_path "$fixture" "$captured_parent" "$mutated_tree")"
    if grep -Fq 'no verification evidence was published' "$record_dir.output" \
        && [[ ! -e "$captured_state" && ! -e "$mutated_state" ]]; then
        pass 'index mutation during verification publishes no evidence'
    else
        fail 'index mutation left verification evidence or the wrong verdict'
    fi
fi

printf 'precommit-staged-tree: %d passed, %d failed\n' "$passes" "$failures"
[[ "$failures" -eq 0 ]]
