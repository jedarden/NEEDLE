#!/usr/bin/env bash
# Reject executable references to the retired beads-rust CLI in docs and
# validate the repository's Forgejo-source/GitHub-artifact documentation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOC_ROOT="${NEEDLE_DOCUMENTATION_ROOT:-$REPO_ROOT}"

# This matches command-shaped uses of either retired CLI while not matching
# words such as "braces" or backend names in ordinary prose. A historical
# document may retain an incident's exact command, but it must carry the
# visible notice and machine-readable marker below so readers do not mistake
# it for current guidance.
retired_br="$(printf '\142\162')"
retired_bf="$(printf '\142\146')"
RETIRED_BR_PATTERN="(^|[^[:alnum:]_])${retired_br}([^[:alnum:]_]|$)"
RETIRED_BF_PATTERN="(^|[^[:alnum:]_])${retired_bf}[[:space:]]+(init|sync|create|list|show|update|close|reopen|release|claim|ready|dep|label|ref|capabilities|doctor|mend|export|import|checkpoint|archive|help|version)([^[:alnum:]_-]|$)"
HISTORICAL_NOTICE='Historical/non-operational reference:'
HISTORICAL_MARKERS=(
  "<!-- retired-${retired_br}: historical-only -->"
  '<!-- retired-cli: historical-only -->'
)

has_historical_marker() {
  local file="$1" marker
  for marker in "${HISTORICAL_MARKERS[@]}"; do
    grep -Fq "$marker" "$file" && return 0
  done
  return 1
}

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
  matches="$(grep -nE "$RETIRED_BR_PATTERN|$RETIRED_BF_PATTERN" "$file" || true)"
  [[ -n "$matches" ]] || continue

  if ! grep -Fq "$HISTORICAL_NOTICE" "$file" || \
      ! has_historical_marker "$file"; then
    printf 'retired CLI command reference in operational documentation: %s\n%s\n' \
      "${file#"$DOC_ROOT/"}" "$matches" >&2
    failures=$((failures + 1))
  fi
done < <(documentation_files)

if [[ "$failures" -ne 0 ]]; then
  cat >&2 <<'EOF'
Use the current `bead` CLI in operational documentation. If an exact retired
command must remain as historical evidence, add the visible historical notice
and a retired-cli historical-only marker used by the repository.
EOF
  exit 1
fi

if [[ -f "$DOC_ROOT/README.md" && -f "$DOC_ROOT/install.sh" ]]; then
  NEEDLE_RELEASE_AUTHORITY_ROOT="$DOC_ROOT" \
    "$SCRIPT_DIR/check-release-authority.sh"
fi

echo 'documentation check: no unmarked retired CLI commands'
