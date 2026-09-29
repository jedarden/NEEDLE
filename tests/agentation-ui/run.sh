#!/usr/bin/env bash
# Verify that the Agentation guard passes for NEEDLE's current no-UI tree and
# blocks a future UI entrypoint until browser mount coverage is wired.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-agentation-ui.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-agentation-ui-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

output="$(bash "$CHECKER")"
grep -Fq 'no production UI entrypoints exist' <<<"$output"
echo "PASS: current UI inventory has no entrypoints"

mkdir -p "$TMP_ROOT/future-ui"
touch "$TMP_ROOT/future-ui/index.html"
if output="$(AGENTATION_SCAN_ROOT="$TMP_ROOT/future-ui" bash "$CHECKER" 2>&1)"; then
  echo "FAIL: future UI entrypoint was accepted without browser coverage" >&2
  exit 1
fi
grep -Fq 'browser mount coverage' <<<"$output"
grep -Fq "document.getElementById('agentation-root')" <<<"$output"
grep -Fq 'react-dom/client, and react/jsx-runtime' <<<"$output"
grep -Fq 'before the agentation.js module' <<<"$output"
echo "PASS: future UI entrypoints require import-map and browser-mount coverage"
