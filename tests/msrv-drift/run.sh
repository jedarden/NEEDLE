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
  mkdir -p "$destination/scripts" "$destination/crates/needle-learning"
  cp "$REPO_ROOT/scripts/check-msrv-drift.sh" "$destination/scripts/"
  cp "$REPO_ROOT/Cargo.toml" "$destination/"
  cp "$REPO_ROOT/rust-toolchain.toml" "$destination/"
  cp "$REPO_ROOT/crates/needle-learning/Cargo.toml" "$destination/crates/needle-learning/"
}

expect_failure() {
  local name="$1" expected="$2" root="$3" output
  if output="$(bash "$root/scripts/check-msrv-drift.sh" 2>&1)"; then
    bad "$name was accepted"
  elif grep -Fq -- "$expected" <<<"$output"; then
    ok "$name"
  else
    bad "$name failed without '$expected': $output"
  fi
}

valid="$TMP_ROOT/valid"
fresh_fixture "$valid"
if bash "$valid/scripts/check-msrv-drift.sh" >/dev/null; then
  ok "aligned MSRV contract passes"
else
  bad "aligned MSRV contract passes"
fi

case_root="$TMP_ROOT/root-drift"
fresh_fixture "$case_root"
sed -i 's/^rust-version = "1.95"/rust-version = "1.88"/' "$case_root/Cargo.toml"
expect_failure "root manifest MSRV below the toolchain pin is rejected" \
  "Cargo.toml declares rust-version 1.88 but rust-toolchain.toml pins 1.95.0" \
  "$case_root"

case_root="$TMP_ROOT/member-drift"
fresh_fixture "$case_root"
sed -i 's/^rust-version = "1.95"/rust-version = "1.75"/' \
  "$case_root/crates/needle-learning/Cargo.toml"
expect_failure "workspace member MSRV drift is rejected" \
  "crates/needle-learning/Cargo.toml declares rust-version 1.75" \
  "$case_root"

case_root="$TMP_ROOT/toolchain-bump-alone"
fresh_fixture "$case_root"
sed -i 's/channel = "1.95.0"/channel = "1.97.1"/' "$case_root/rust-toolchain.toml"
expect_failure "toolchain bump without a rust-version bump is rejected" \
  "expected rust-version \"1.97\"" "$case_root"

case_root="$TMP_ROOT/missing-declaration"
fresh_fixture "$case_root"
sed -i '/^rust-version = /d' "$case_root/crates/needle-learning/Cargo.toml"
expect_failure "missing rust-version is rejected" \
  "expected exactly one package rust-version in crates/needle-learning/Cargo.toml" \
  "$case_root"

echo "MSRV drift checker tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
