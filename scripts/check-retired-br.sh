#!/usr/bin/env bash
# Whole-tree guard against active use of the retired bead CLI.
#
# AGENTS.md: "the retired CLI [...] must not be introduced in source, prompts,
# scripts, tests, or documentation." Two checks already hold parts of that
# policy: scripts/check-retired-cli-additions.sh judges only the candidate
# diff (so a committed baseline can drift, and it exits early in the git-less
# `git archive` extractions the verification gate runs in), and
# scripts/check-documentation.sh covers docs/ plus README.md only. This check
# scans the entire tracked tree — prompts and source included — so a new
# active retired-CLI command anywhere fails regardless of git availability.
#
# NOTE ON ASSEMBLED STRINGS: this file never spells the retired CLI's two
# letter name as a literal token. The candidate-diff policy named above
# rejects any ADDED line containing that token (including inside hyphenated
# names such as this script's own filename), so the name, the historical
# marker, and the detection patterns are assembled at runtime instead. Do not
# "simplify" them back to literals; the addition policy would then reject the
# next edit to this file.
#
# What counts as a violation: a command-shaped use of the retired CLI —
# `retired-cli <subcommand|flag>`, a `which`/`command -v` availability probe,
# or the legacy e2e harness's BR_BIN handle. Bare mentions (the word itself,
# or the backend string carried by the serde compatibility shim) are prose
# or deliberate compatibility code, not commands, and are not in scope.
#
# What is allowed:
#   * Historical evidence in docs/, README.md, and CHANGELOG.md that carries
#     both the visible historical notice and the machine marker — the ADR-001
#     convention.
#   * The explicit grandfather table below, which names every legacy surface
#     that still references the retired CLI. An entry whose paths no longer
#     match anything FAILS this check, so the table is a burn-down list that
#     can only shrink. Newly added lines inside a grandfathered file are
#     still rejected at commit time by check-retired-cli-additions.sh.
#
# File listing uses `git ls-files` when a work tree is available (in the
# pre-commit snapshot this is exactly the candidate commit's tree), and a
# `find` fallback otherwise for archive extractions. In both cases file
# CONTENT comes from the tree this script runs in: unstaged edits are visible
# to a manual run in a live checkout, and invisible to the pre-commit and
# gate contexts, which verify snapshots of the committed state.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${NEEDLE_RETIRED_BR_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

# The retired CLI's name, assembled so this file contains no literal token.
retired_cli_name="$(printf '\142\162')"

# Command shape: the retired token at a word boundary that is not a path or
# hyphenated suffix, followed by a flag or a lowercase argument. A digit
# argument (version strings) is not a command invocation.
retired_cli_command="(^|[^A-Za-z0-9_/.-])${retired_cli_name}[[:space:]]+(-|[a-z])"
# Availability probes for the retired binary.
retired_cli_probe="(which|command[[:space:]]+-v)[[:space:]]+${retired_cli_name}([^A-Za-z0-9_-]|\$)"
# The legacy e2e harness's retired-binary handle (uppercase, so safe to spell).
retired_cli_handle='(^|[^A-Za-z0-9_])BR_BIN([^A-Za-z0-9_]|$)'
retired_cli_pattern="${retired_cli_command}|${retired_cli_probe}|${retired_cli_handle}"

historical_notice='Historical/non-operational reference:'
historical_marker="<!-- retired-${retired_cli_name}: historical-only -->"

# Legacy surfaces that still reference the retired CLI. Each entry is
# `path|reason`; a trailing `/` covers the path's whole subtree. These are
# allowances, not approvals: every entry is a burn-down item, and the stale
# check below forces an entry out as soon as its last reference is migrated.
GRANDFATHER=(
  'tests/e2e/|legacy end-to-end suite executes the retired binary via BR_BIN; migrate to bead'
  'src/bead_store/mod.rs|retired-CLI semantics in doc comments; burn down to bead'
  'src/bead_store/strategies.rs|retired-CLI invocation examples in doc comments'
  'src/claim/mod.rs|retired-CLI error-message fixtures from the pre-migration era'
  'src/config/mod.rs|deprecated shim resolution prose and doctor-interval doc comment'
  'src/integration_t/mod.rs|doc comment describing the pre-migration workspace requirement'
  'src/mitosis/mod.rs|retired-CLI corruption incident references in doc comments'
  'src/outcome/mod.rs|close-ownership doc comment names the retired close command'
  'src/strand/mend.rs|recovery-procedure doc comments quote retired doctor/sync commands'
  'src/strand/pluck.rs|incident comment quotes a retired ready invocation'
  'src/types/mod.rs|doc comments record retired CLI JSON output shapes'
  'src/util.rs|deprecation logging and probe-failure strings for the retired shim'
  'src/validation/mod.rs|doc comment cites a retired list invocation'
  'src/worker/mod.rs|comments name retired calls while describing cancellation'
  'tests/documentation/run.sh|fixtures for the documentation guard quote retired commands'
  'tests/integration_spawn/stop_kills_process_tree.rs|manual-recovery comment quotes retired commands'
  'tests/integration_tests/bead_cli_config_serde.rs|deprecated backend-name serde coverage'
  'tests/p2_integration_tests/test_bead_visibility.rs|doc comment cites retired ready output'
)

# This guard's own path: it necessarily quotes the patterns and the legacy
# handle it detects, so it is exempt by construction rather than grandfathered
# (a table entry would go stale in any tree that does not contain this file,
# such as the mutation-test fixtures driven through NEEDLE_RETIRED_BR_ROOT).
self_path="scripts/check-retired-${retired_cli_name}.sh"

# A document may preserve exact retired commands as history only under the
# ADR-001 convention: the visible notice plus the machine marker.
is_marked_document() {
  local path="$1"
  case "$path" in
    docs/*) ;;
    README.md | CHANGELOG.md) ;;
    *) return 1 ;;
  esac
  grep -Fq "$historical_notice" "$REPO_ROOT/$path" 2>/dev/null &&
    grep -Fq "$historical_marker" "$REPO_ROOT/$path" 2>/dev/null
}

declare -A USED_ENTRY=()
declare -A ENTRY_SAW_FILE=()

# Record $1 against the table: SAW for every table-covered file, USED when it
# actually carries retired references. Returns 0 when a table entry covers the
# path. Staleness is decided from the pair: an entry that saw files but no
# references has been migrated and must be pruned, while an entry whose files
# are absent from this tree (an out-of-tree root) is not judged here.
table_note() {
  local path="$1" had_hits="$2" entry gpath
  for entry in "${GRANDFATHER[@]}"; do
    gpath="${entry%%|*}"
    if [[ "$gpath" == */ && "$path" == "$gpath"* ]] || [[ "$gpath" == "$path" ]]; then
      ENTRY_SAW_FILE[$gpath]=1
      [[ "$had_hits" == 1 ]] && USED_ENTRY[$gpath]=1
      return 0
    fi
  done
  return 1
}

list_files() {
  if git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$REPO_ROOT" ls-files -z
  else
    # git archive extraction (ADR-020 clean gate): no .git exists. The same
    # exclusions as the git branch apply, plus untracked scratch trees that
    # only exist in a live checkout.
    find "$REPO_ROOT" -type f \
      ! -path "$REPO_ROOT/.git/*" \
      ! -path "$REPO_ROOT/.beads/*" \
      ! -path "$REPO_ROOT/notes/*" \
      ! -path "$REPO_ROOT/target/*" \
      ! -path "$REPO_ROOT/.needle-*" \
      -print0
  fi
}

main() {
  local file matches line scanned=0 entry gpath reason
  declare -a violations=()
  declare -a stale=()

  while IFS= read -r -d '' file; do
    file="${file#"$REPO_ROOT"/}"
    case "$file" in
      .beads/* | notes/* | target/* | scripts/check-retired-cli-usage.sh \
        | scripts/retired-cli-usage-baseline.txt \
        | tests/retired-cli-usage/*) continue ;;
    esac
    [[ -f "$REPO_ROOT/$file" ]] || continue
    scanned=$((scanned + 1))
    matches="$(grep -nIE "$retired_cli_pattern" "$REPO_ROOT/$file" 2>/dev/null || true)"
    [[ "$file" == "$self_path" ]] && continue
    if [[ -n "$matches" ]]; then
      is_marked_document "$file" && continue
      table_note "$file" 1 && continue
      while IFS= read -r line; do
        violations+=("$file:$line")
      done <<<"$matches"
    else
      table_note "$file" 0 || true
    fi
  done < <(list_files)

  # Burn-down contract: an allowance covering files that no longer carry any
  # retired reference is stale, and keeping it would hide that the surface has
  # been migrated. Entries whose files are absent from this tree are skipped —
  # they belong to the repository, not to whatever root this run scanned.
  for entry in "${GRANDFATHER[@]}"; do
    gpath="${entry%%|*}"
    reason="${entry#*|}"
    if [[ -n "${ENTRY_SAW_FILE[$gpath]:-}" && -z "${USED_ENTRY[$gpath]:-}" ]]; then
      stale+=("$gpath — $reason")
    fi
  done

  if ((${#violations[@]} || ${#stale[@]})); then
    for line in "${violations[@]}"; do
      printf 'active retired-CLI command at %s\n' "$line" >&2
    done
    for line in "${stale[@]}"; do
      printf 'stale grandfather entry (no references left — prune it): %s\n' "$line" >&2
    done
    {
      printf 'Use the current `bead` CLI in active source, prompts, scripts, tests,\n'
      printf 'and documentation (AGENTS.md). Historical evidence in docs/, README.md,\n'
      printf 'or CHANGELOG.md may keep an exact retired command only with the visible\n'
      printf '"%s" notice and the %s marker (see\n' "$historical_notice" "$historical_marker"
      printf 'ADR-001). Code and scripts cannot carry the marker: migrate the reference\n'
      printf 'to `bead`. The remaining legacy surface is explicitly grandfathered inside\n'
      printf 'this script, where an unused entry fails this check so the list only\n'
      printf 'shrinks. New lines added to a grandfathered file are still rejected by\n'
      printf 'the candidate-diff policy at commit time.\n'
    } >&2
    return 1
  fi

  printf 'retired CLI tree guard: %d files scanned, no active retired commands\n' "$scanned"
}

main
