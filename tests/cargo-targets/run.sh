#!/usr/bin/env bash
# Regression tests for Cargo's explicit integration-target boundary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
BASE_DOCKERFILE="$REPO_ROOT/ci/Dockerfile.ci"
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

# P2/P3 fixtures need bead-rs at test runtime. Keep its builder-image release
# contract exact and ordered: HTTPS download, checksum verification, executable
# install, then a version assertion. A runtime fallback would multiply this
# download across every parallel cargo-target pod.
grep -Fq 'ARG BEAD_RS_VERSION=0.2.6' "$BASE_DOCKERFILE" \
  || fail 'base image must pin bead-rs 0.2.6'
[[ "$(grep -c '^ARG BEAD_RS_VERSION=' "$BASE_DOCKERFILE")" -eq 1 ]] \
  || fail 'base image must declare exactly one bead-rs version pin'
grep -Fq 'ARG BEAD_RS_SHA256=15324894af38a8ffce8ad54da47ac067c7fd198779a7744f967bcf56a9aa3aee' "$BASE_DOCKERFILE" \
  || fail 'base image must pin the bead-rs 0.2.6 checksum'
[[ "$(grep -c '^ARG BEAD_RS_SHA256=' "$BASE_DOCKERFILE")" -eq 1 ]] \
  || fail 'base image must declare exactly one bead-rs checksum pin'
grep -Fq '"https://github.com/jedarden/bead-rs/releases/download/v${BEAD_RS_VERSION}/bead-x86_64-unknown-linux-gnu"' "$BASE_DOCKERFILE" \
  || fail 'base image must download the pinned bead-rs Linux release over HTTPS'
grep -Fq 'echo "${BEAD_RS_SHA256}  /tmp/bead-dl" | sha256sum --check --strict' "$BASE_DOCKERFILE" \
  || fail 'base image must verify bead-rs before installation'
grep -Fq 'install -m 0755 /tmp/bead-dl /usr/local/bin/bead' "$BASE_DOCKERFILE" \
  || fail 'base image must install bead-rs with executable permissions'
grep -Fq 'test "$(bead --version | cut -d'\'' '\'' -f1-2)" = "bead ${BEAD_RS_VERSION}"' "$BASE_DOCKERFILE" \
  || fail 'base image must assert the exact installed bead-rs version'

bead_download_line="$(grep -nF '"https://github.com/jedarden/bead-rs/releases/download/v${BEAD_RS_VERSION}/bead-x86_64-unknown-linux-gnu"' "$BASE_DOCKERFILE" | cut -d: -f1)"
bead_checksum_line="$(grep -nF 'echo "${BEAD_RS_SHA256}  /tmp/bead-dl" | sha256sum --check --strict' "$BASE_DOCKERFILE" | cut -d: -f1)"
bead_install_line="$(grep -nF 'install -m 0755 /tmp/bead-dl /usr/local/bin/bead' "$BASE_DOCKERFILE" | cut -d: -f1)"
bead_assert_line="$(grep -nF 'test "$(bead --version | cut -d'\'' '\'' -f1-2)" = "bead ${BEAD_RS_VERSION}"' "$BASE_DOCKERFILE" | cut -d: -f1)"
[[ "$bead_download_line" -lt "$bead_checksum_line" && \
   "$bead_checksum_line" -lt "$bead_install_line" && \
   "$bead_install_line" -lt "$bead_assert_line" ]] || fail \
  'base image must verify bead-rs before installing and version-checking it'

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
echo 'PASS: base image pins and verifies bead-rs 0.2.6 before installation'
