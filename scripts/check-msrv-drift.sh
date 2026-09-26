#!/usr/bin/env bash
# Keep the package's declared MSRV, needle-ci's verify-msrv toolchain, and the
# documented authoritative checks aligned. The default mirrors the current
# MSRV_TOOLCHAIN in declarative-config's needle-ci WorkflowTemplate; callers in
# that lane can pass its actual value through the environment.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
DOC="$REPO_ROOT/CLAUDE.md"
CI_TOOLCHAIN="${MSRV_TOOLCHAIN:-1.88.0}"

if [[ ! -f "$MANIFEST" || ! -f "$DOC" ]]; then
  echo "ERROR: expected Cargo.toml and CLAUDE.md under $REPO_ROOT" >&2
  exit 1
fi

if [[ ! "$CI_TOOLCHAIN" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "ERROR: MSRV_TOOLCHAIN must be a full Rust version (got '$CI_TOOLCHAIN')" >&2
  exit 1
fi
CI_RELEASE="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"

DECLARED_MSRV="$(awk '
  /^\[package\][[:space:]]*$/ { package = 1; next }
  /^\[/ { package = 0 }
  package && /^[[:space:]]*rust-version[[:space:]]*=/ {
    value = $0
    sub(/^[^=]*=[[:space:]]*/, "", value)
    gsub(/[[:space:]"]/, "", value)
    print value
    count++
  }
  END { if (count != 1) exit 1 }
' "$MANIFEST")" || {
  echo "ERROR: expected exactly one package rust-version in $MANIFEST" >&2
  exit 1
}

if [[ ! "$DECLARED_MSRV" =~ ^([0-9]+)\.([0-9]+)(\.([0-9]+))?$ ]]; then
  echo "ERROR: unsupported Cargo.toml rust-version '$DECLARED_MSRV'" >&2
  exit 1
fi
DECLARED_RELEASE="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"

if [[ "$DECLARED_RELEASE" != "$CI_RELEASE" ]]; then
  echo "ERROR: Cargo.toml declares Rust $DECLARED_MSRV but needle-ci MSRV_TOOLCHAIN is $CI_TOOLCHAIN" >&2
  echo "Update the manifest and needle-ci toolchain together." >&2
  exit 1
fi

BUILD_DOC='RUSTUP_TOOLCHAIN=<msrv> cargo build --workspace --all-targets'
TEST_DOC='RUSTUP_TOOLCHAIN=<msrv> cargo test --workspace --lib'
if ! grep -Fqx "$BUILD_DOC" "$DOC"; then
  echo "ERROR: CLAUDE.md is missing the authoritative MSRV build command: $BUILD_DOC" >&2
  exit 1
fi
if ! grep -Fqx "$TEST_DOC" "$DOC"; then
  echo "ERROR: CLAUDE.md is missing the authoritative MSRV test command: $TEST_DOC" >&2
  exit 1
fi

echo "MSRV contract aligned: Cargo.toml $DECLARED_MSRV, needle-ci $CI_TOOLCHAIN"
echo "Authoritative checks: cargo build --workspace --all-targets; cargo test --workspace --lib"
