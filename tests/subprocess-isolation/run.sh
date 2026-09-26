#!/usr/bin/env bash
# Run the static subprocess isolation guard without building the full
# integration_spawn test target. The Rust source uses only the standard
# library, so rustc can execute its tests quickly in the fast DoD lane.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

guard_dir="$(mktemp -d "$REPO_ROOT/.needle-subprocess-isolation.XXXXXX")"
guard_binary="$guard_dir/guard-tests"
cleanup() {
  if [[ -e "$guard_binary" ]]; then
    unlink "$guard_binary"
  fi
  rmdir "$guard_dir"
}
trap cleanup EXIT

CARGO_MANIFEST_DIR="$REPO_ROOT" rustc --edition=2021 --test \
  tests/integration_spawn/subprocess_isolation.rs \
  -o "$guard_binary"
"$guard_binary"
