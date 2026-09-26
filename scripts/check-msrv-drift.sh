#!/usr/bin/env bash
# Keep the package's declared MSRV, pinned build toolchain, needle-ci's
# verify-msrv toolchain, and the documented authoritative checks aligned. The
# build toolchain is intentionally newer than the MSRV. The default mirrors the
# current MSRV_TOOLCHAIN in declarative-config's needle-ci WorkflowTemplate;
# callers in that lane can pass its actual value through the environment.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO_ROOT/Cargo.toml"
TOOLCHAIN_FILE="$REPO_ROOT/rust-toolchain.toml"
DOC="$REPO_ROOT/CLAUDE.md"
CI_TOOLCHAIN="${MSRV_TOOLCHAIN:-1.88.0}"

if [[ ! -f "$MANIFEST" || ! -f "$TOOLCHAIN_FILE" || ! -f "$DOC" ]]; then
  echo "ERROR: expected Cargo.toml, rust-toolchain.toml, and CLAUDE.md under $REPO_ROOT" >&2
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
MSRV_MAJOR="${BASH_REMATCH[1]}"
MSRV_MINOR="${BASH_REMATCH[2]}"
MSRV_PATCH="${BASH_REMATCH[4]:-0}"

BUILD_TOOLCHAIN="$(awk '
  /^\[toolchain\][[:space:]]*$/ { toolchain = 1; next }
  /^\[/ { toolchain = 0 }
  toolchain && /^[[:space:]]*channel[[:space:]]*=/ {
    value = $0
    sub(/^[^=]*=[[:space:]]*/, "", value)
    gsub(/[[:space:]]/, "", value)
    gsub(/"/, "", value)
    print value
    count++
  }
  END { if (count != 1) exit 1 }
' "$TOOLCHAIN_FILE")" || {
  echo "ERROR: expected exactly one [toolchain] channel in $TOOLCHAIN_FILE" >&2
  exit 1
}

if [[ ! "$BUILD_TOOLCHAIN" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "ERROR: rust-toolchain.toml must pin a full Rust version (got '$BUILD_TOOLCHAIN')" >&2
  exit 1
fi
BUILD_MAJOR="${BASH_REMATCH[1]}"
BUILD_MINOR="${BASH_REMATCH[2]}"
BUILD_PATCH="${BASH_REMATCH[3]}"

version_is_newer() {
  local left_major="$1" left_minor="$2" left_patch="$3"
  local right_major="$4" right_minor="$5" right_patch="$6"
  if (( 10#$left_major != 10#$right_major )); then
    (( 10#$left_major > 10#$right_major ))
  elif (( 10#$left_minor != 10#$right_minor )); then
    (( 10#$left_minor > 10#$right_minor ))
  else
    (( 10#$left_patch > 10#$right_patch ))
  fi
}

if ! version_is_newer \
    "$BUILD_MAJOR" "$BUILD_MINOR" "$BUILD_PATCH" \
    "$MSRV_MAJOR" "$MSRV_MINOR" "$MSRV_PATCH"; then
  echo "ERROR: rust-toolchain.toml pins Rust $BUILD_TOOLCHAIN, which must be newer than Cargo.toml MSRV $DECLARED_MSRV" >&2
  exit 1
fi

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

echo "MSRV contract aligned: Cargo.toml $DECLARED_MSRV, rust-toolchain.toml $BUILD_TOOLCHAIN, needle-ci $CI_TOOLCHAIN"
echo "Authoritative checks: cargo build --workspace --all-targets; cargo test --workspace --lib"
