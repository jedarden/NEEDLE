#!/usr/bin/env bash
# Reject new command-shaped uses of the retired bead CLIs in active repository
# surfaces. Existing source/script/test narration is grandfathered in a
# reviewable baseline; documentation is never grandfathered and must either be
# rewritten or carry an explicit historical-only marker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${NEEDLE_RETIRED_CLI_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
BASELINE_REL="scripts/retired-cli-usage-baseline.txt"
BASELINE="$REPO_ROOT/$BASELINE_REL"

UPDATE_BASELINE=false
if [[ "${1:-}" == "--update-baseline" ]]; then
  UPDATE_BASELINE=true
elif [[ -n "${1:-}" ]]; then
  echo "usage: $0 [--update-baseline]" >&2
  exit 2
fi

# Match runnable command forms, not bare backend names in prose, identifiers,
# version output, or bead IDs. The command list is deliberately conservative:
# a new command-shaped form is actionable, while ordinary retirement prose is
# not.
RETIRED_PATTERN='(^|[^[:alnum:]_])(br|bf)[[:space:]]+(init|sync|create|list|show|update|close|reopen|release|claim|ready|dep|label|ref|capabilities|doctor|mend|export|import|checkpoint|archive|help|version)([^[:alnum:]_-]|$)'
RETIRED_PROBE_PATTERN='(which|command[[:space:]]+-v)[[:space:]]+(br|bf)([^[:alnum:]_-]|$)'
RETIRED_HANDLE_PATTERN='(^|[^[:alnum:]_])(BR|BF)_BIN([^[:alnum:]_]|$)'

HISTORICAL_NOTICE='Historical/non-operational reference:'
HISTORICAL_MARKERS=(
  '<!-- retired-br: historical-only -->'
  '<!-- retired-cli: historical-only -->'
)

SCAN_DIRS=(src prompt prompts scripts tests docs)
SCAN_FILES=(README.md CLAUDE.md AGENTS.md CHANGELOG.md)

is_excluded() {
  case "$1" in
    scripts/check-retired-cli-usage.sh | \
      scripts/retired-cli-usage-baseline.txt | \
      tests/retired-cli-usage/*)
      return 0
      ;;
    *) return 1 ;;
  esac
}

is_document() {
  case "$1" in
    docs/* | README.md | CLAUDE.md | AGENTS.md | CHANGELOG.md) return 0 ;;
    *) return 1 ;;
  esac
}

is_baseline_eligible() {
  case "$1" in
    docs/* | README.md | CLAUDE.md | AGENTS.md | CHANGELOG.md) return 1 ;;
    *) return 0 ;;
  esac
}

carries_historical_marker() {
  local file="$1" marker
  grep -Fq "$HISTORICAL_NOTICE" "$file" || return 1
  for marker in "${HISTORICAL_MARKERS[@]}"; do
    grep -Fq "$marker" "$file" && return 0
  done
  return 1
}

list_files() {
  local dir path rel name
  for dir in "${SCAN_DIRS[@]}"; do
    [[ -d "$REPO_ROOT/$dir" ]] || continue
    while IFS= read -r -d '' path; do
      rel="${path#"$REPO_ROOT"/}"
      is_excluded "$rel" || printf '%s\0' "$rel"
    done < <(find "$REPO_ROOT/$dir" -type f -print0)
  done
  for name in "${SCAN_FILES[@]}"; do
    [[ -f "$REPO_ROOT/$name" ]] || continue
    is_excluded "$name" || printf '%s\0' "$name"
  done
}

# Emit stable path + normalized matching line signatures. Documentation and
# root operational guidance are omitted when building the baseline so that an
# accidental new command there cannot be grandfathered.
scan_signatures() {
  local omit_locked="$1" rel file hit line
  while IFS= read -r -d '' rel; do
    file="$REPO_ROOT/$rel"
    if is_document "$rel"; then
      if [[ "$omit_locked" == true ]] && ! is_baseline_eligible "$rel"; then
        continue
      fi
      carries_historical_marker "$file" && continue
    fi
    while IFS= read -r hit; do
      line="${hit#*:}"
      printf '%s\t%s\n' "$rel" "$(printf '%s' "$line" | tr -s '[:space:]' ' ')"
    done < <(
      grep -nE "$RETIRED_PATTERN|$RETIRED_PROBE_PATTERN|$RETIRED_HANDLE_PATTERN" \
        "$file" 2>/dev/null || true
    )
  done < <(list_files | LC_ALL=C sort -zu)
}

if [[ "$UPDATE_BASELINE" == true ]]; then
  scan_signatures true | LC_ALL=C sort -u >"$BASELINE"
  echo "baseline updated: $BASELINE_REL ($(wc -l <"$BASELINE") entries)"
  exit 0
fi

if [[ ! -f "$BASELINE" ]]; then
  echo "ERROR: $BASELINE_REL is missing; run $0 --update-baseline" >&2
  exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/needle-retired-cli-XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
CURRENT="$WORK/current"
BASELINED="$WORK/baselined"
LC_ALL=C sort -u "$BASELINE" >"$BASELINED"
scan_signatures false | LC_ALL=C sort -u >"$CURRENT"

NEW="$(LC_ALL=C comm -13 "$BASELINED" "$CURRENT")"
if [[ -n "$NEW" ]]; then
  printf 'retired bead CLI references that are not historical or baselined:\n' >&2
  while IFS=$'\t' read -r rel line; do
    [[ -n "$rel" ]] || continue
    printf '  %s: %s\n' "$rel" "$line" >&2
  done <<<"$NEW"
  cat >&2 <<'EOF'

Use the current `bead` CLI. Source, prompts, scripts, and tests must not add
new `br` commands or unscoped `bf` commands. Active documentation must be
rewritten; exact historical commands are allowed only in a document carrying
the visible Historical/non-operational reference notice and a retired-cli
historical-only marker. Existing non-documentation narration belongs in the
reviewed baseline and should be removed when the surrounding code is migrated.
EOF
  exit 1
fi

STALE="$(LC_ALL=C comm -23 "$BASELINED" "$CURRENT" | grep -c . || true)"
if [[ "$STALE" -gt 0 ]]; then
  echo "note: $STALE baseline entries no longer match anything; regenerate with $0 --update-baseline"
fi
echo "retired CLI usage check: no new br or unscoped bf command references"
