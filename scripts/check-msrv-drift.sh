#!/usr/bin/env bash
# Keep every workspace manifest's declared MSRV equal to the pinned build
# toolchain. NEEDLE's MSRV IS the rust-toolchain.toml release (major.minor):
# every needle-ci lane builds with that pin, so the declaration is enforced by
# ordinary CI and needs no separate MSRV lane. Bump the pin and every
# rust-version in the same change.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLCHAIN_FILE="$REPO_ROOT/rust-toolchain.toml"

if [[ ! -f "$REPO_ROOT/Cargo.toml" || ! -f "$TOOLCHAIN_FILE" ]]; then
  echo "ERROR: expected Cargo.toml and rust-toolchain.toml under $REPO_ROOT" >&2
  exit 1
fi

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
BUILD_RELEASE="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"

shopt -s nullglob
manifests=("$REPO_ROOT/Cargo.toml" "$REPO_ROOT"/crates/*/Cargo.toml)

for manifest in "${manifests[@]}"; do
  label="${manifest#"$REPO_ROOT"/}"
  declared="$(awk '
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
  ' "$manifest")" || {
    echo "ERROR: expected exactly one package rust-version in $label" >&2
    exit 1
  }
  if [[ "$declared" != "$BUILD_RELEASE" ]]; then
    echo "ERROR: $label declares rust-version $declared but rust-toolchain.toml pins $BUILD_TOOLCHAIN (expected rust-version \"$BUILD_RELEASE\")" >&2
    echo "Move the toolchain pin and every rust-version together." >&2
    exit 1
  fi
done

echo "MSRV contract aligned: rust-version $BUILD_RELEASE in ${#manifests[@]} manifest(s) = rust-toolchain.toml $BUILD_TOOLCHAIN"
