#!/usr/bin/env bash
# Regression tests for Cargo's explicit integration-target boundary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
BASE_DOCKERFILE="$REPO_ROOT/ci/Dockerfile.ci"
DEPS_DOCKERFILE="$REPO_ROOT/ci/Dockerfile.ci-deps"
TOOLCHAIN_FILE="$REPO_ROOT/rust-toolchain.toml"
NEXTEST_CONFIG="$REPO_ROOT/.config/nextest.toml"
CI_VERSION_FILE="$REPO_ROOT/ci/VERSION"
EXPECTED_TARGETS="$(printf '%s\n' \
  escalation_ladder \
  integration_spawn \
  integration_tests \
  nt07_admission_budgets \
  nt07_admission_policy \
  nt07_executable_admission \
  nt07_impact_contract \
  nt07_impact_scoring \
  nt07_proposal_contract \
  nt10_policy_precedence \
  nt45_failure_evidence_capture \
  nt50_exception_lessons \
  nt51_adapter_usage_capture \
  nt51_gateway_health \
  nt52_state_dir_isolation \
  nt53_improvement_proposals \
  nt54_proposal_admission \
  nt55_impact_receipts \
  nt56_improvements_cli \
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
grep -Fq 'COPY .config/nextest.toml ./.config/nextest.toml' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must copy the nextest profile used by the archive producer'
grep -Fq 'ln -s /opt/needle-ci-target /workspace/target' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must warm through the workflow target symlink'
grep -Fq "printf '%s\\n' \"\$actual_toolchain\" > /opt/needle-ci-toolchain-version" "$DEPS_DOCKERFILE" \
  || fail 'dependency image must record the compiler release outside target/'
grep -Fq 'test "$source_toolchain" = "$actual_toolchain"' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must verify source/compiler toolchain parity'
grep -Fq 'test -d /opt/needle-ci-target/debug/deps' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must verify warmed dependency artifacts exist'
grep -Fq 'test -s /opt/needle-ci-nextest-cache-contract' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must publish a non-empty nextest cache contract marker'
grep -Fq 'rm /workspace/target' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must remove only its warm-up target symlink'
if grep -Eq 'rm[[:space:]].*(/opt/needle-ci-target|(-r|-f|--recursive)[^;]*(/workspace/)?target)' "$DEPS_DOCKERFILE"; then
  fail 'dependency image must preserve /opt/needle-ci-target after warm-up'
fi
if grep -Eq '^COPY[[:space:]]+src/' "$DEPS_DOCKERFILE"; then
  fail 'dependency image must not COPY volatile NEEDLE source into the warm layer'
fi

[[ "$(grep -cF 'CARGO_BUILD_JOBS=2 cargo-nextest nextest archive \' "$DEPS_DOCKERFILE")" -eq 1 ]] \
  || fail 'dependency image must invoke the exact nextest archive warm-up once'
grep -Fq -- '--archive-file /tmp/needle-nextest-dependency-warm.tar.zst' "$DEPS_DOCKERFILE" \
  || fail 'dependency warm-up must write its disposable nextest archive outside the image target'
grep -Fq -- '--profile ci' "$DEPS_DOCKERFILE" \
  || fail 'dependency warm-up must use the producer nextest profile'
grep -Fq -- '--locked' "$DEPS_DOCKERFILE" \
  || fail 'dependency warm-up must lock dependency resolution'
grep -Fq -- '--lib' "$DEPS_DOCKERFILE" \
  || fail 'dependency warm-up must include the library test binary'
for target in $EXPECTED_TARGETS; do
  [[ "$(grep -cF -- "--test $target" "$DEPS_DOCKERFILE")" -eq 1 ]] \
    || fail "dependency warm-up must include declared test target $target exactly once"
done
if grep -Eq -- '--(tests|all-targets|bins|examples|benches)([[:space:]]|$)' "$DEPS_DOCKERFILE"; then
  fail 'dependency warm-up must not silently broaden the producer target set'
fi
if grep -Eq '^[[:space:]]*cargo build([[:space:]]|$)' "$DEPS_DOCKERFILE"; then
  fail 'dependency image must not warm the incompatible Cargo dev profile'
fi
grep -Fq 'rm -f /tmp/needle-nextest-dependency-warm.tar.zst' "$DEPS_DOCKERFILE" \
  || fail 'dependency image must remove its disposable warm-up archive in the same layer'

for marker_fragment in \
  'schema=needle-nextest-dependency-cache/v1' \
  'cargo_toml_sha256=' \
  'cargo_lock_sha256=' \
  'rust_toolchain_sha256=' \
  'nextest_config_sha256=' \
  'rust_release=' \
  'nextest_release=' \
  'nextest_binary_sha256=' \
  'cargo_profile=test' \
  'nextest_profile=ci' \
  'features=default' \
  'target_triple=' \
  'target_set=lib,integration_spawn,integration_tests,p2_integration_tests,p3_integration_tests,real_br_integration_tests,escalation_ladder,nt45_failure_evidence_capture,nt50_exception_lessons,nt51_adapter_usage_capture,nt10_policy_precedence,nt51_gateway_health,nt52_state_dir_isolation,nt07_admission_policy,nt07_admission_budgets,nt53_improvement_proposals,nt54_proposal_admission,nt55_impact_receipts,nt56_improvements_cli,nt07_proposal_contract,nt07_impact_contract,nt07_impact_scoring,nt07_executable_admission' \
  'workspace_root=/workspace' \
  'target_dir=/opt/needle-ci-target' \
  'cargo_build_jobs=2'; do
  grep -Fq "$marker_fragment" "$DEPS_DOCKERFILE" \
    || fail "dependency cache marker is missing $marker_fragment"
done

copy_line="$(grep -nF 'COPY Cargo.toml Cargo.lock rust-toolchain.toml ./' "$DEPS_DOCKERFILE" | cut -d: -f1)"
config_copy_line="$(grep -nF 'COPY .config/nextest.toml ./.config/nextest.toml' "$DEPS_DOCKERFILE" | cut -d: -f1)"
build_line="$(grep -nF 'CARGO_BUILD_JOBS=2 cargo-nextest nextest archive \' "$DEPS_DOCKERFILE" | cut -d: -f1)"
archive_remove_line="$(grep -nF 'rm -f /tmp/needle-nextest-dependency-warm.tar.zst' "$DEPS_DOCKERFILE" | cut -d: -f1)"
deps_line="$(grep -nF 'test -d /opt/needle-ci-target/debug/deps' "$DEPS_DOCKERFILE" | cut -d: -f1)"
marker_line="$(grep -nF '} > /opt/needle-ci-nextest-cache-contract' "$DEPS_DOCKERFILE" | cut -d: -f1)"
unlink_line="$(grep -nF 'rm /workspace/target' "$DEPS_DOCKERFILE" | cut -d: -f1)"
[[ -n "$copy_line" && -n "$config_copy_line" && -n "$build_line" && \
   "$copy_line" -lt "$build_line" && "$config_copy_line" -lt "$build_line" ]] || fail \
  'dependency image must copy every profile input before the nextest warm-up'
[[ -n "$archive_remove_line" && -n "$deps_line" && -n "$marker_line" && -n "$unlink_line" && \
   "$build_line" -lt "$archive_remove_line" && "$archive_remove_line" -lt "$deps_line" && \
   "$deps_line" -lt "$marker_line" && "$marker_line" -lt "$unlink_line" ]] || fail \
  'dependency image must remove the archive, verify artifacts, record the marker, then unlink target'

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

# cargo-nextest is part of the runner image contract, not something each
# archive producer/consumer is allowed to download independently. Verify the
# release archive and the extracted executable before installation.
grep -Fq 'ARG CARGO_NEXTEST_VERSION=0.9.144' "$BASE_DOCKERFILE" \
  || fail 'base image must pin cargo-nextest 0.9.144'
grep -Fq 'ARG CARGO_NEXTEST_ARCHIVE_SHA256=20ed0a7d3d6f8dda9bb1b0bcb5838aea5784d3e2360746280868996d709dde0a' "$BASE_DOCKERFILE" \
  || fail 'base image must pin the cargo-nextest 0.9.144 archive checksum'
grep -Fq 'ARG CARGO_NEXTEST_BINARY_SHA256=92e0def8d330c7bb295175a998ad73f65670dc3d216d1ae7a28f286d130ce1ad' "$BASE_DOCKERFILE" \
  || fail 'base image must pin the extracted cargo-nextest 0.9.144 checksum'
[[ "$(grep -c '^ARG CARGO_NEXTEST_VERSION=' "$BASE_DOCKERFILE")" -eq 1 ]] \
  || fail 'base image must declare exactly one cargo-nextest version pin'
[[ "$(grep -c '^ARG CARGO_NEXTEST_ARCHIVE_SHA256=' "$BASE_DOCKERFILE")" -eq 1 ]] \
  || fail 'base image must declare exactly one cargo-nextest archive checksum'
[[ "$(grep -c '^ARG CARGO_NEXTEST_BINARY_SHA256=' "$BASE_DOCKERFILE")" -eq 1 ]] \
  || fail 'base image must declare exactly one cargo-nextest binary checksum'
grep -Fq '"https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${CARGO_NEXTEST_VERSION}/${archive}"' "$BASE_DOCKERFILE" \
  || fail 'base image must download the pinned cargo-nextest release over HTTPS'
grep -Fq 'echo "${CARGO_NEXTEST_ARCHIVE_SHA256}  /tmp/${archive}" | sha256sum --check --strict' "$BASE_DOCKERFILE" \
  || fail 'base image must verify the cargo-nextest archive before extraction'
grep -Fq 'echo "${CARGO_NEXTEST_BINARY_SHA256}  ${root}/cargo-nextest" | sha256sum --check --strict' "$BASE_DOCKERFILE" \
  || fail 'base image must verify the extracted cargo-nextest binary before installation'
grep -Fq 'install -m 0755 "${root}/cargo-nextest" /usr/local/bin/cargo-nextest' "$BASE_DOCKERFILE" \
  || fail 'base image must install cargo-nextest with executable permissions'
grep -Fq 'cargo-nextest --version | grep -Fx "release: ${CARGO_NEXTEST_VERSION}"' "$BASE_DOCKERFILE" \
  || fail 'base image must assert the installed cargo-nextest release'

nextest_download_line="$(grep -nF '"https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${CARGO_NEXTEST_VERSION}/${archive}"' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_archive_checksum_line="$(grep -nF 'echo "${CARGO_NEXTEST_ARCHIVE_SHA256}  /tmp/${archive}" | sha256sum --check --strict' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_extract_line="$(grep -nF 'tar -xzf "/tmp/${archive}" -C "${root}"' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_binary_checksum_line="$(grep -nF 'echo "${CARGO_NEXTEST_BINARY_SHA256}  ${root}/cargo-nextest" | sha256sum --check --strict' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_install_line="$(grep -nF 'install -m 0755 "${root}/cargo-nextest" /usr/local/bin/cargo-nextest' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_assert_line="$(grep -nF 'cargo-nextest --version | grep -Fx "release: ${CARGO_NEXTEST_VERSION}"' "$BASE_DOCKERFILE" | cut -d: -f1)"
toolchain_install_line="$(grep -nF 'RUN rustup toolchain install 1.95.0 \' "$BASE_DOCKERFILE" | cut -d: -f1)"
toolchain_end_line="$(grep -nF '    --target aarch64-unknown-linux-gnu' "$BASE_DOCKERFILE" | cut -d: -f1)"
nextest_version_line="$(grep -nF 'ARG CARGO_NEXTEST_VERSION=0.9.144' "$BASE_DOCKERFILE" | cut -d: -f1)"
workdir_line="$(grep -nF 'WORKDIR /workspace' "$BASE_DOCKERFILE" | cut -d: -f1)"
[[ "$nextest_download_line" -lt "$nextest_archive_checksum_line" && \
   "$nextest_archive_checksum_line" -lt "$nextest_extract_line" && \
   "$nextest_extract_line" -lt "$nextest_binary_checksum_line" && \
   "$nextest_binary_checksum_line" -lt "$nextest_install_line" && \
   "$nextest_install_line" -lt "$nextest_assert_line" ]] || fail \
  'base image must verify cargo-nextest archive and binary before installing it'
[[ -n "$toolchain_install_line" && -n "$toolchain_end_line" && \
   "$toolchain_install_line" -lt "$toolchain_end_line" && \
   "$toolchain_end_line" -lt "$nextest_version_line" && \
   "$nextest_version_line" -lt "$nextest_download_line" && \
   "$nextest_assert_line" -lt "$workdir_line" ]] || fail \
  'base image must add cargo-nextest after the pinned Rust toolchain layer and before WORKDIR'

[[ "$(tr -d '\n' < "$CI_VERSION_FILE")" == "0.1.12" ]] \
  || fail 'ci/VERSION must move with the exact-profile dependency image contents'
grep -Fq 'nextest-version = { required = "0.9.144" }' "$NEXTEST_CONFIG" \
  || fail 'nextest config must set the minimum supported runner version'
grep -Fq '[profile.ci]' "$NEXTEST_CONFIG" \
  || fail 'nextest config must declare the CI profile'
grep -Fq 'fail-fast = false' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must preserve aggregate failure reporting'
grep -Fq 'retries = 0' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must disable implicit retries'
grep -Fq 'flaky-result = "fail"' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must fail on flaky results'
grep -Fq 'test-threads = 2' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must retain two-way target concurrency'
grep -Fq 'failure-output = "immediate-final"' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must show failures immediately and in the final summary'
grep -Fq 'success-output = "never"' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must suppress successful test output'
grep -Fq 'status-level = "pass"' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must report live passing-test status'
grep -Fq 'final-status-level = "fail"' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile final summary must focus on failures'
grep -Fq '[profile.ci.junit]' "$NEXTEST_CONFIG" \
  || fail 'nextest CI profile must emit JUnit'
grep -Fq 'path = "junit.xml"' "$NEXTEST_CONFIG" \
  || fail 'nextest JUnit path must remain stable for artifact publication'
grep -Fq 'report-skipped = "none"' "$NEXTEST_CONFIG" \
  || fail 'nextest shards must not duplicate excluded tests as skipped JUnit cases'

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
if [[ -f "$REPO_ROOT/crates/needle-learning/Cargo.toml" ]]; then
  mkdir -p "$probe_root/crates/needle-learning"
  ln -s "$REPO_ROOT/crates/needle-learning/Cargo.toml" "$probe_root/crates/needle-learning/Cargo.toml"
  ln -s "$REPO_ROOT/crates/needle-learning/src" "$probe_root/crates/needle-learning/src"
  ln -s "$REPO_ROOT/crates/needle-learning/tests" "$probe_root/crates/needle-learning/tests"
fi
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

echo 'PASS: Cargo reports only the explicit integration-test roots'
echo 'PASS: a stray tests/scratch.rs is not auto-discovered'
echo 'PASS: dependency-image stubs cover every declared Cargo target'
echo 'PASS: dependency image preserves its target tree outside /workspace'
echo "PASS: rustc release matches the source toolchain pin ($source_toolchain)"
echo 'PASS: base image pins and verifies bead-rs 0.2.6 before installation'
echo 'PASS: base image pins and verifies cargo-nextest 0.9.144 before installation'
echo 'PASS: nextest CI profile emits stable, non-duplicated JUnit output'
echo 'PASS: CI image version is 0.1.12'

"$REPO_ROOT/tests/nextest-shard-plan/run.sh"
