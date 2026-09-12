#!/usr/bin/env bash
# Regression tests for Cargo's explicit integration-target boundary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
DEPS_DOCKERFILE="$REPO_ROOT/ci/Dockerfile.ci-deps"
TOOLCHAIN_FILE="$REPO_ROOT/rust-toolchain.toml"
EXPECTED_TARGETS="$(printf '%s\n' \
  integration_spawn \
  integration_tests \
  p2_integration_tests \
  p3_integration_tests \
  real_br_integration_tests)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

test_targets() {
  cargo metadata \
    --manifest-path "$1" \
    --format-version 1 \
    --no-deps \
    --locked \
    --offline \
    | jq -r '.packages[] | select(.name == "needle")
      | .targets[] | select(.kind | index("test")) | .name' \
    | sort
}

manifest_target_paths() {
  cargo metadata \
    --manifest-path "$1" \
    --format-version 1 \
    --no-deps \
    --locked \
    --offline \
    | jq -r '.packages[] | select(.name == "needle")
      | .targets[].src_path' \
    | while IFS= read -r path; do
        realpath --relative-to="$REPO_ROOT" "$path"
      done \
    | sort -u
}

grep -Eq '^autotests[[:space:]]*=[[:space:]]*false([[:space:]]*(#.*)?)?$' "$MANIFEST" \
  || fail 'Cargo.toml must disable integration-test auto-discovery'

# The image must build its warm target tree at the same absolute path Cargo
# sees in workflow pods. /workspace itself is hidden by a volume mount, so the
# durable directory and toolchain marker both live outside it; only the symlink
# is removed from the image layer after warm-up.
grep -Fq 'COPY Cargo.toml Cargo.lock rust-toolchain.toml ./' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must copy rust-toolchain.toml before warming Cargo'
grep -Fq 'ln -s /opt/needle-ci-target /workspace/target' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must warm through the workflow target symlink'
grep -Fq "printf '%s\\n' \"\$actual_toolchain\" > /opt/needle-ci-toolchain-version" "$DEPS_DOCKERFILE" \
  || fail 'dependency image must record the compiler release outside target/'
grep -Fq 'test "$source_toolchain" = "$actual_toolchain"' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must verify source/compiler toolchain parity'
grep -Fq 'test -d /opt/needle-ci-target/debug/deps' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must verify warmed dependency artifacts exist'
grep -Fq 'rm /workspace/target' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must remove only its warm-up target symlink'
if grep -Eq 'rm[[:space:]].*(/opt/needle-ci-target|(-r|-f|--recursive)[^;]*(/workspace/)?target)' "$DEPS_DOCKERFILE"; then
  fail 'dependency image must preserve /opt/needle-ci-target after warm-up'
fi

copy_line="$(grep -nF 'COPY Cargo.toml Cargo.lock rust-toolchain.toml ./' "$DEPS_DOCKERFILE" | cut -d: -f1)"
build_line="$(grep -nF '    cargo build && \' "$DEPS_DOCKERFILE" | cut -d: -f1)"
deps_line="$(grep -nF 'test -d /opt/needle-ci-target/debug/deps' "$DEPS_DOCKERFILE" | cut -d: -f1)"
unlink_line="$(grep -nF 'rm /workspace/target' "$DEPS_DOCKERFILE" | cut -d: -f1)"
[[ -n "$copy_line" && -n "$build_line" && "$copy_line" -lt "$build_line" ]] || fail \
  'dependency image must copy rust-toolchain.toml before the first Cargo build'
[[ -n "$deps_line" && -n "$unlink_line" && "$build_line" -lt "$deps_line" && "$deps_line" -lt "$unlink_line" ]] || fail \
  'dependency image must verify warmed artifacts before removing the target symlink'

source_toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TOOLCHAIN_FILE")"
actual_toolchain="$(rustc -vV | sed -n 's/^release: //p')"
[[ -n "$source_toolchain" ]] || fail 'rust-toolchain.toml has no exact channel'
[[ -n "$actual_toolchain" ]] || fail 'rustc -vV has no release field'
[[ "$source_toolchain" == "$actual_toolchain" ]] || fail \
  "local rustc release $actual_toolchain differs from source pin $source_toolchain"

actual_targets="$(test_targets "$MANIFEST")"
[[ "$actual_targets" == "$EXPECTED_TARGETS" ]] || fail \
  "repository test targets differ from the intended roots: $(tr '\n' ' ' <<<"$actual_targets")"

# The dependency-warming image replaces each declared target with a trivial
# source file before invoking Cargo. Keep that synthetic package in lockstep
# with the real manifest: a missing path makes the image fail before it can
# publish the with-deps tag.
while IFS= read -r target_path; do
  grep -Fq -- "> $target_path" "$DEPS_DOCKERFILE" || fail \
    "ci/Dockerfile.ci-deps does not create declared target $target_path"
done < <(manifest_target_paths "$MANIFEST")

# Exercise Cargo's discovery behavior with a real stray file, but do it in a
# temporary package so this regression never mutates the shared checkout.
probe_root="$(mktemp -d "${TMPDIR:-/tmp}/needle-cargo-targets-XXXXXX")"
trap 'rm -rf -- "$probe_root"' EXIT

cp "$MANIFEST" "$probe_root/Cargo.toml"
ln -s "$REPO_ROOT/Cargo.lock" "$probe_root/Cargo.lock"
ln -s "$REPO_ROOT/src" "$probe_root/src"
ln -s "$REPO_ROOT/benches" "$probe_root/benches"
mkdir "$probe_root/tests"
while IFS= read -r target; do
  ln -s "$REPO_ROOT/tests/$target.rs" "$probe_root/tests/$target.rs"
done <<<"$EXPECTED_TARGETS"

printf '%s\n' '#[test]' 'fn must_not_be_discovered() {}' \
  > "$probe_root/tests/scratch.rs"

probe_targets="$(test_targets "$probe_root/Cargo.toml")"
[[ "$probe_targets" == "$EXPECTED_TARGETS" ]] || fail \
  "tests/scratch.rs changed Cargo metadata: $(tr '\n' ' ' <<<"$probe_targets")"
[[ "$probe_targets" != *scratch* ]] || fail 'Cargo auto-discovered tests/scratch.rs'

echo 'PASS: Cargo reports only the five explicit integration-test roots'
echo 'PASS: a stray tests/scratch.rs is not auto-discovered'
echo 'PASS: dependency-image stubs cover every declared Cargo target'
echo 'PASS: dependency image preserves its target tree outside /workspace'
echo "PASS: rustc release matches the source toolchain pin ($source_toolchain)"
