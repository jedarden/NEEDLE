#!/usr/bin/env bash
# Compile Cargo's --lib test target, run the complete harness under an exec
# audit, and reject every successful execve after the harness bootstrap.
set -euo pipefail

export LC_ALL=C
export CARGO_TERM_COLOR=never
umask 077

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
operator_home="${HOME:?HOME must be set}"
scratch_root="$(mktemp -d /tmp/needle-lpp.XXXXXX)"

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

for tool in awk find grep mktemp rustup sed sort strace tail; do
    command -v "$tool" >/dev/null 2>&1 \
        || die "required command is unavailable: $tool"
done

# Keep the checked source inventory visible and reviewable. The policy scanner
# understands Rust's cfg(test) boundary and reports direct process constructors
# in unit-test code. Its exact path:line-list output must match the catalog.
mapfile -d '' source_files < <(find "$repo_root/src" -type f -name '*.rs' -print0 | sort -z)
process_sites="$({
    cd "$repo_root"
    awk -f scripts/check-test-policy.awk "${source_files[@]#"$repo_root/"}"
} | awk -F '\t' '$2 == "process" { print $1 ":" $4 }' | sort)"
[[ -n "$process_sites" ]] || process_sites="(none)"

documented_sites="$(sed -n \
    '/<!-- lib-process-purity-inventory:start -->/,/<!-- lib-process-purity-inventory:end -->/ {
        /<!-- lib-process-purity-inventory:/d
        p
    }' "$repo_root/docs/process-spawning-test-catalog.md")"
if [[ "$process_sites" != "$documented_sites" ]]; then
    printf 'FAIL: checked unit-test process-site inventory differs from the catalog\n' >&2
    printf 'documented:\n%s\nactual:\n%s\n' "$documented_sites" "$process_sites" >&2
    exit 1
fi
printf 'PASS: checked unit-test process-site inventory is synchronized\n'

# Resolve the real toolchain before changing HOME. The host's cargo executable
# is a CI-submit wrapper; rustup's resolved binary builds this local executable
# contract directly and works when the audit runs with a synthetic HOME.
cargo_bin="$(rustup which cargo)"
rustc_bin="$(rustup which rustc)"
rustdoc_bin="$(rustup which rustdoc)"
[[ -x "$cargo_bin" ]] || die "rustup did not resolve an executable cargo"
[[ -x "$rustc_bin" ]] || die "rustup did not resolve an executable rustc"
[[ -x "$rustdoc_bin" ]] || die "rustup did not resolve an executable rustdoc"

build_log="$scratch_root/build.log"
if ! (
    cd "$repo_root"
    RUSTC="$rustc_bin" RUSTDOC="$rustdoc_bin" \
        "$cargo_bin" test --locked --lib --no-run
) >"$build_log" 2>&1; then
    tail -40 "$build_log" >&2
    die "failed to compile the --lib test target"
fi

test_binary="$(sed -n \
    's/^  Executable unittests src\/lib\.rs (\(.*\))$/\1/p' \
    "$build_log" | tail -1)"
[[ -n "$test_binary" ]] || {
    tail -40 "$build_log" >&2
    die "cargo did not report the --lib test executable"
}
[[ -x "$test_binary" ]] || die "reported --lib test target is not executable"

synthetic_home="$scratch_root/home"
suite_tmp="$scratch_root/t"
mkdir -p \
    "$synthetic_home/.config" \
    "$synthetic_home/.cache" \
    "$synthetic_home/.local/share" \
    "$suite_tmp"

trace="$scratch_root/execve.trace"
test_log="$scratch_root/test.log"
set +e
HOME="$synthetic_home" \
XDG_CONFIG_HOME="$synthetic_home/.config" \
XDG_CACHE_HOME="$synthetic_home/.cache" \
XDG_DATA_HOME="$synthetic_home/.local/share" \
TMPDIR="$suite_tmp" \
    strace -f -qq -e trace=execve,execveat -o "$trace" \
    "$test_binary" >"$test_log" 2>&1
test_status=$?
set -e

if [[ $test_status -ne 0 ]]; then
    tail -60 "$test_log" >&2
    die "--lib test target failed under the exec audit (status $test_status)"
fi

result_line="$(grep '^test result: ok\.' "$test_log" | tail -1)"
[[ -n "$result_line" ]] || {
    tail -40 "$test_log" >&2
    die "--lib test target did not report a successful test result"
}

execution_calls="$(awk '/(^|[[:space:]])(execve|execveat)\(/ { count++ } END { print count + 0 }' "$trace")"
if [[ "$execution_calls" -ne 1 ]]; then
    printf 'FAIL: expected one execution syscall (the harness bootstrap), observed %s\n' \
        "$execution_calls" >&2
    awk '/(^|[[:space:]])(execve|execveat)\(/ { print; shown++; if (shown == 20) exit }' "$trace" >&2
    exit 1
fi

if ! awk -v binary="$test_binary" \
    '/(^|[[:space:]])(execve|execveat)\(/ { found = index($0, "\"" binary "\"") > 0 && $0 ~ / = 0$/ } END { exit !found }' \
    "$trace"; then
    awk '/(^|[[:space:]])(execve|execveat)\(/ { print }' "$trace" >&2
    die "the sole execution syscall was not a successful --lib harness bootstrap"
fi

printf 'PASS: %s\n' "$result_line"
printf 'PASS: --lib emitted 0 child execution attempts (1 execve: harness bootstrap)\n'
