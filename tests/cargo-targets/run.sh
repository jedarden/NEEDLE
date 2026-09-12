#!/usr/bin/env bash
# Regression tests for Cargo's explicit integration-target boundary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
DEPS_DOCKERFILE="$REPO_ROOT/ci/Dockerfile.ci-deps"
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
