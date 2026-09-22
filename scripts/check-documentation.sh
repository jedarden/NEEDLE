#!/usr/bin/env bash
# Reject executable references to the retired beads_rust `br` CLI in docs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOC_ROOT="${NEEDLE_DOCUMENTATION_ROOT:-$REPO_ROOT}"

# This matches the standalone CLI name, including bare invocations and paths
# that end in `/br`, while not matching words such as "braces". A historical
# document may retain an incident's exact command, but it must carry the
# visible notice and machine-readable marker below so readers do not mistake
# it for current guidance.
RETIRED_BR_PATTERN='(^|[^[:alnum:]_])br([^[:alnum:]_]|$)'
HISTORICAL_NOTICE='Historical/non-operational reference:'
HISTORICAL_MARKER='<!-- retired-br: historical-only -->'

documentation_files() {
  [[ -d "$DOC_ROOT/docs" ]] && find "$DOC_ROOT/docs" -type f \( \
    -name '*.md' -o -name '*.markdown' -o -name '*.rst' -o -name '*.txt' \
    -o -name '*.sh' -o -name '*.yaml' -o -name '*.yml' \
  \) -print0
  for root_file in README.md; do
    [[ -f "$DOC_ROOT/$root_file" ]] && printf '%s\0' "$DOC_ROOT/$root_file"
  done
}

failures=0
while IFS= read -r -d '' file; do
  matches="$(grep -nE "$RETIRED_BR_PATTERN" "$file" || true)"
  [[ -n "$matches" ]] || continue

  if ! grep -Fq "$HISTORICAL_NOTICE" "$file" || \
      ! grep -Fq "$HISTORICAL_MARKER" "$file"; then
    printf 'retired br command reference in operational documentation: %s\n%s\n' \
      "${file#"$DOC_ROOT/"}" "$matches" >&2
    failures=$((failures + 1))
  fi
done < <(documentation_files)

if [[ "$failures" -ne 0 ]]; then
  cat >&2 <<'EOF'
Use the current `bead` CLI in operational documentation. If an exact `br`
command must remain as historical evidence, add this visible notice and marker:

> Historical/non-operational reference: this document preserves retired `br`
> commands from an earlier implementation or incident. Do not run them; use
> the current `bead` CLI for active work.
<!-- retired-br: historical-only -->
EOF
  exit 1
fi

echo 'documentation check: no unmarked retired br commands'
