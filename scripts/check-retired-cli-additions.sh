#!/usr/bin/env bash
# Reject newly added references to the retired CLI in active repository files.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${NEEDLE_RETIRED_CLI_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
if ! git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "retired CLI addition check: no Git diff available; mutation tests cover the policy"
  exit 0
fi

tmp_file="$(mktemp "${TMPDIR:-/tmp}/needle-retired-cli-diff.XXXXXX")"
trap 'rm -f -- "$tmp_file" "$tmp_file.hits"' EXIT

# In the pre-commit snapshot HEAD is the current parent and the worktree is the
# captured index tree. Comparing against HEAD therefore checks exactly the
# candidate commit while ignoring concurrent, unstaged edits in the checkout.
if ! git -C "$REPO_ROOT" diff --no-ext-diff --no-color --unified=0 HEAD -- >"$tmp_file"; then
  echo "retired CLI addition check: unable to read the candidate diff" >&2
  exit 2
fi

awk -v root="$REPO_ROOT" '
  function in_scope(path) {
    if (path == "scripts/check-retired-cli-usage.sh" ||
        path ~ /^tests\/retired-cli-usage\// ||
        path == "scripts/retired-cli-usage-baseline.txt") {
      return 0
    }
    return path ~ /^(src|prompt|prompts|scripts|tests|docs)\// ||
      path == ".needle.yaml" || path == "AGENTS.md" ||
      path == "README.md" || path == "CHANGELOG.md" || path == "CLAUDE.md" ||
      path ~ /(^|\/)(prompt|prompts)\//
  }
  function is_document(path) {
    return path ~ /^docs\/.*\.(md|markdown|rst|txt)$/ ||
      path == "README.md" || path == "CHANGELOG.md" ||
      path == "AGENTS.md" || path == "CLAUDE.md"
  }
  BEGIN {
    cli = sprintf("%c%c", 98, 114)
    legacy_cli = sprintf("%c%c", 98, 102)
    cli_token = "(^|[^[:alnum:]_])" cli "([^[:alnum:]_]|$)"
    legacy_words = "(init|sync|create|list|show|update|close|reopen|release|claim|ready|dep|label|ref|capabilities|doctor|mend|export|import|checkpoint|archive|help|version)"
    legacy_command = "(^|[^[:alnum:]_])" legacy_cli "[[:space:]]+" legacy_words "([^[:alnum:]_-]|$)"
    legacy_probe = "(which|command[[:space:]]+-v)[[:space:]]+" legacy_cli "([^[:alnum:]_-]|$)"
    legacy_handle_name = sprintf("%c%c%c%c%c%c", 66, 70, 95, 66, 73, 78)
    legacy_handle = "(^|[^[:alnum:]_])" legacy_handle_name "([^[:alnum:]_]|$)"
    notice = "Historical/non-operational reference:"
    marker = "<!-- retired-" cli ": historical-only -->"
    unified_marker = "<!-- retired-cli: historical-only -->"
  }
  /^\+\+\+ b\// {
    path = substr($0, 7)
    next
  }
  /^diff --git / { path = ""; next }
  /^\+/ && path != "/dev/null" && in_scope(path) {
    line = substr($0, 2)
    if (line ~ cli_token || line ~ legacy_command || line ~ legacy_probe || line ~ legacy_handle) {
      allowlist_check = "grep -Fq " shq(notice) " " shq(root "/" path) " && (grep -Fq " shq(marker) " " shq(root "/" path) " || grep -Fq " shq(unified_marker) " " shq(root "/" path) ")"
      historical = is_document(path) && system(allowlist_check) == 0
      if (!historical) {
        printf "%s\t%s\n", path, line
      }
    }
  }
  function shq(value,   result, i, ch) {
    result = "\047"
    for (i = 1; i <= length(value); i++) {
      ch = substr(value, i, 1)
      if (ch == "\047") result = result "\047\\\047\047"
      else result = result ch
    }
    return result "\047"
  }
' "$tmp_file" >"$tmp_file.hits"

if [[ -s "$tmp_file.hits" ]]; then
  while IFS=$'\t' read -r path line; do
    printf 'new retired CLI reference in active file %s:\n  %s\n' "$path" "$line" >&2
  done <"$tmp_file.hits"
  cat >&2 <<'EOF'
Use the current `bead` CLI in active source, prompts, scripts, tests, and docs.
An exact historical reference in documentation is allowed only when that file
contains both the visible historical notice and the retired-cli marker.
EOF
  exit 1
fi

echo "retired CLI addition check: no active references added"
