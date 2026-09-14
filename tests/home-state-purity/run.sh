#!/usr/bin/env bash
# Prove that the complete Cargo test topology cannot mutate an inherited
# ~/.needle/state directory. The operator's real HOME is never inspected: the
# suite inherits a seeded synthetic HOME and the state tree is compared after
# every test process has exited.
set -euo pipefail

export LC_ALL=C
umask 077

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch_root="$(mktemp -d "${TMPDIR:-/tmp}/needle-home-state-purity.XXXXXX")"

cleanup() {
    if [[ -d "$scratch_root" ]]; then
        find "$scratch_root" -depth -delete
    fi
}
trap cleanup EXIT

die() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

for tool in cmp cp diff find mktemp rustup sha256sum tar; do
    command -v "$tool" >/dev/null 2>&1 || die "required command is unavailable: $tool"
done

# GNU tar's stable ordering and normalized ownership/timestamps turn a tree
# into a byte-comparable record. File contents, names, kinds, modes, hard-link
# relationships, and symlink targets remain significant.
snapshot_tree() {
    local tree="$1"
    local archive="$2"

    [[ -d "$tree" ]] || die "state tree does not exist: $tree"
    tar \
        --sort=name \
        --format=gnu \
        --mtime=@0 \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -C "$tree" \
        -cf "$archive" \
        .
}

assert_snapshot_unchanged() {
    local state_tree="$1"
    local before_archive="$2"
    local before_tree="$3"
    local after_archive="$4"

    snapshot_tree "$state_tree" "$after_archive"
    if cmp -s "$before_archive" "$after_archive"; then
        return 0
    fi

    printf 'FAIL: synthetic ~/.needle/state changed during verification\n' >&2
    # Keep diagnostics useful but bounded. diff exit 1 means "different" and
    # is expected on this path; an archive mismatch remains fatal even if diff
    # itself cannot describe an unusual filesystem object.
    diff -qr --no-dereference "$before_tree" "$state_tree" 2>&1 \
        | sed -n '1,20p' >&2 || true
    return 1
}

seed_state_tree() {
    local state_tree="$1"

    mkdir -p "$state_tree/heartbeats" "$state_tree/nested"
    printf '{"worker":"sentinel","status":"healthy"}\n' \
        > "$state_tree/heartbeats/preexisting.json"
    printf 'prefix\000binary\377suffix\n' > "$state_tree/nested/payload.bin"
    ln -s ../nested/payload.bin "$state_tree/heartbeats/payload-link"
    chmod 0640 "$state_tree/nested/payload.bin"
}

expect_detector_rejects() {
    local mutation="$1"
    local case_root="$scratch_root/detector-$mutation"
    local state_tree="$case_root/home/.needle/state"
    local before_tree="$case_root/before-tree"
    local before_archive="$case_root/before.tar"
    local after_archive="$case_root/after.tar"

    seed_state_tree "$state_tree"
    mkdir -p "$before_tree"
    cp -a "$state_tree/." "$before_tree/"
    snapshot_tree "$state_tree" "$before_archive"

    case "$mutation" in
        create)
            printf 'unexpected\n' > "$state_tree/created-by-test"
            ;;
        modify)
            printf 'changed\n' > "$state_tree/nested/payload.bin"
            ;;
        delete)
            find "$state_tree/heartbeats/preexisting.json" -delete
            ;;
        *)
            die "unknown detector mutation: $mutation"
            ;;
    esac

    if assert_snapshot_unchanged \
        "$state_tree" "$before_archive" "$before_tree" "$after_archive" \
        >/dev/null 2>&1; then
        die "state detector accepted a $mutation mutation"
    fi
    printf 'PASS: state detector rejects %s\n' "$mutation"
}

verify_detector_contract() {
    local case_root="$scratch_root/detector-unchanged"
    local state_tree="$case_root/home/.needle/state"
    local before_tree="$case_root/before-tree"
    local before_archive="$case_root/before.tar"
    local after_archive="$case_root/after.tar"

    seed_state_tree "$state_tree"
    mkdir -p "$before_tree"
    cp -a "$state_tree/." "$before_tree/"
    snapshot_tree "$state_tree" "$before_archive"
    assert_snapshot_unchanged \
        "$state_tree" "$before_archive" "$before_tree" "$after_archive" \
        || die "state detector rejected an unchanged tree"
    printf 'PASS: state detector accepts an unchanged tree\n'

    expect_detector_rejects create
    expect_detector_rejects modify
    expect_detector_rejects delete
}

if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--detector-contract-only" ) ]]; then
    die "usage: $0 [--detector-contract-only]"
fi

verify_detector_contract
if [[ $# -eq 1 ]]; then
    exit 0
fi

# Resolve the real toolchain before changing HOME. Invoking the cargo wrapper
# from PATH could recursively submit this verification to CI; rustup's resolved
# binaries execute the suite locally and continue to work under synthetic HOME.
cargo_bin="$(rustup which cargo)"
rustc_bin="$(rustup which rustc)"
rustdoc_bin="$(rustup which rustdoc)"
[[ -x "$cargo_bin" ]] || die "rustup did not resolve an executable cargo"
[[ -x "$rustc_bin" ]] || die "rustup did not resolve an executable rustc"
[[ -x "$rustdoc_bin" ]] || die "rustup did not resolve an executable rustdoc"

real_home="${HOME:?HOME must be set so the Rust cache can be preserved}"
cargo_home="${CARGO_HOME:-$real_home/.cargo}"
rustup_home="${RUSTUP_HOME:-$real_home/.rustup}"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$repo_root/$target_dir"
fi

synthetic_home="$scratch_root/suite-home"
state_tree="$synthetic_home/.needle/state"
before_tree="$scratch_root/suite-before-tree"
before_archive="$scratch_root/suite-before.tar"
after_archive="$scratch_root/suite-after.tar"

mkdir -p \
    "$synthetic_home/.cache" \
    "$synthetic_home/.config" \
    "$synthetic_home/.local/state" \
    "$synthetic_home/tmp" \
    "$synthetic_home/workspaces"
seed_state_tree "$state_tree"
mkdir -p "$before_tree"
cp -a "$state_tree/." "$before_tree/"
snapshot_tree "$state_tree" "$before_archive"

printf 'Running complete Cargo test topology under synthetic HOME\n'
set +e
(
    cd "$repo_root"
    env \
        -u NEEDLE_EVENTS \
        -u NEEDLE_HEARTBEATS \
        -u NEEDLE_HOME \
        -u NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT \
        -u NEEDLE_SUPERVISOR_HEARTBEAT_PATH \
        -u NEEDLE_SUPERVISOR_SOCKET \
        -u NEEDLE_SUPERVISOR_SOCKET_PATH \
        -u NEEDLE_SUPERVISOR__HEARTBEAT_PATH \
        -u NEEDLE_SUPERVISOR__SOCKET_PATH \
        -u NEEDLE_WORKSPACE \
        HOME="$synthetic_home" \
        XDG_CACHE_HOME="$synthetic_home/.cache" \
        XDG_CONFIG_HOME="$synthetic_home/.config" \
        XDG_STATE_HOME="$synthetic_home/.local/state" \
        TMPDIR="$synthetic_home/tmp" \
        CARGO_HOME="$cargo_home" \
        RUSTUP_HOME="$rustup_home" \
        CARGO_TARGET_DIR="$target_dir" \
        RUSTC="$rustc_bin" \
        RUSTDOC="$rustdoc_bin" \
        "$cargo_bin" test --locked
)
suite_status=$?
set -e

state_status=0
assert_snapshot_unchanged \
    "$state_tree" "$before_archive" "$before_tree" "$after_archive" \
    || state_status=$?

if [[ "$suite_status" -ne 0 ]]; then
    printf 'FAIL: complete Cargo test topology exited with status %d\n' \
        "$suite_status" >&2
fi
if [[ "$state_status" -eq 0 ]]; then
    before_sha="$(sha256sum "$before_archive" | awk '{print $1}')"
    after_sha="$(sha256sum "$after_archive" | awk '{print $1}')"
    [[ "$before_sha" == "$after_sha" ]] \
        || die "state archives compared equal but their SHA-256 digests differ"
    printf 'PASS: complete Cargo test topology left synthetic ~/.needle/state byte-identical (%s)\n' \
        "$after_sha"
fi

[[ "$suite_status" -eq 0 && "$state_status" -eq 0 ]]
