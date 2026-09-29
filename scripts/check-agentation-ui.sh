#!/usr/bin/env bash
# NEEDLE currently has no user-facing UI entrypoints. Keep that invariant
# explicit: the first HTML/framework page must add browser coverage that checks
# the per-page import map and #agentation-root mount before this guard passes.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SCAN_ROOT="${AGENTATION_SCAN_ROOT:-$REPO_ROOT}"

if [[ ! -d "$SCAN_ROOT" ]]; then
  echo "Agentation scan root is not a directory: $SCAN_ROOT" >&2
  exit 2
fi

mapfile -t UI_ENTRYPOINTS < <(
  find "$SCAN_ROOT" \
    -type d \( \
      -name .git -o -name .beads -o -name target -o -name tests -o \
      -name docs -o -name canary -o -name '.*' \
    \) -prune -o \
    -type f \( \
      -iname '*.html' -o -iname '*.htm' -o -iname '*.jsx' -o \
      -iname '*.tsx' -o -iname '*.vue' -o -iname '*.astro' -o \
      -iname '*.svelte' \
    \) -print | sort
)

if [[ "${#UI_ENTRYPOINTS[@]}" -eq 0 ]]; then
  echo "Agentation UI guard passed: no production UI entrypoints exist."
  echo "Future pages must add a browser smoke check for #agentation-root and an import map before agentation.js."
  exit 0
fi

echo "Agentation UI guard found UI source files without browser mount coverage:" >&2
printf '  %s\n' "${UI_ENTRYPOINTS[@]}" >&2
cat >&2 <<'EOF'
Before adding a UI page, extend the per-entrypoint browser smoke test to wait
for page load and assert document.getElementById('agentation-root') exists.
Each page must map react, react-dom, react-dom/client, and react/jsx-runtime
before the agentation.js module.
EOF
exit 1
