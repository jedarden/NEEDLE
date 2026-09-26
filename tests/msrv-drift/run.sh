#!/usr/bin/env bash
# Mutation tests for scripts/check-msrv-drift.sh.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-msrv-drift-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0

ok() {
  echo "PASS: $1"
  passes=$((passes + 1))
}

bad() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

fresh_fixture() {
  local destination="$1"
  mkdir -p "$destination/scripts"
  cp "$REPO_ROOT/scripts/check-msrv-drift.sh" "$destination/scripts/"
  cp "$REPO_ROOT/Cargo.toml" "$destination/"
  cp "$REPO_ROOT/rust-toolchain.toml" "$destination/"
  cp "$REPO_ROOT/CLAUDE.md" "$destination/"
}

expect_failure() {
  local name="$1" expected="$2" root="$3" toolchain="${4:-1.88.0}" output
  if output="$(MSRV_TOOLCHAIN="$toolchain" \
      bash "$root/scripts/check-msrv-drift.sh" 2>&1)"; then
    bad "$name was accepted"
  elif grep -Fq -- "$expected" <<<"$output"; then
    ok "$name"
  else
    bad "$name failed without '$expected': $output"
  fi
}

valid="$TMP_ROOT/valid"
fresh_fixture "$valid"
if MSRV_TOOLCHAIN=1.88.0 bash "$valid/scripts/check-msrv-drift.sh" >/dev/null; then
  ok "aligned MSRV contract passes"
else
  bad "aligned MSRV contract passes"
fi

case_root="$TMP_ROOT/toolchain-drift"
fresh_fixture "$case_root"
expect_failure "Cargo MSRV drift from needle-ci is rejected" \
  "Cargo.toml declares Rust 1.88 but needle-ci MSRV_TOOLCHAIN is 1.89.0" \
  "$case_root" 1.89.0

case_root="$TMP_ROOT/build-toolchain-drift"
fresh_fixture "$case_root"
sed -i 's/channel = "1.95.0"/channel = "1.87.0"/' \
  "$case_root/rust-toolchain.toml"
expect_failure "build toolchain older than Cargo MSRV is rejected" \
  "rust-toolchain.toml pins Rust 1.87.0, which must be newer than Cargo.toml MSRV 1.88" \
  "$case_root"

case_root="$TMP_ROOT/missing-build"
fresh_fixture "$case_root"
sed -i 's/RUSTUP_TOOLCHAIN=<msrv> cargo build --workspace --all-targets/cargo build --workspace --all-targets/' \
  "$case_root/CLAUDE.md"
expect_failure "authoritative build command drift is rejected" \
  "missing the authoritative MSRV build command" "$case_root"

case_root="$TMP_ROOT/missing-test"
fresh_fixture "$case_root"
sed -i 's/RUSTUP_TOOLCHAIN=<msrv> cargo test --workspace --lib/cargo test --workspace --lib/' \
  "$case_root/CLAUDE.md"
expect_failure "authoritative test command drift is rejected" \
  "missing the authoritative MSRV test command" "$case_root"

echo "MSRV drift checker tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
