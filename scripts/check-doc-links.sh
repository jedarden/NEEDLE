#!/usr/bin/env bash
# Deterministic documentation navigation check.
#
# Fails when:
#   1. an internal Markdown link (docs/** plus the repository README) points
#      at a path that does not exist, or escapes the repository root;
#   2. the documentation index (docs/README.md) links the same target twice;
#   3. an ADR or research document on disk is missing from the index;
#   4. the index's documented inventory counts disagree with the tree.
#
# Scope and non-goals are documented in docs/ci/doc-navigation-check.md:
# anchors inside a target file are not validated, external URLs are not
# fetched, and links inside fenced code blocks are ignored.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOC_ROOT="${NEEDLE_DOCUMENTATION_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
INDEX="$DOC_ROOT/docs/README.md"

declare -a FAILURES=()

record() {
  FAILURES+=("$1")
}

# Lexically normalize an absolute-or-relative path against a base directory.
# Pure bash so the checker runs in any archive without GNU realpath.
needle_normalize_path() {
  local base="$1" relative="$2" part rest out="/"
  rest="$base/$relative"
  local -a stack=()
  while [[ -n "$rest" ]]; do
    part="${rest%%/*}"
    if [[ "$rest" == */* ]]; then
      rest="${rest#*/}"
    else
      rest=""
    fi
    case "$part" in
      '' | '.') ;;
      '..')
        if (( ${#stack[@]} > 0 )); then
          unset "stack[$(( ${#stack[@]} - 1 ))]"
        fi
        ;;
      *) stack+=("$part") ;;
    esac
  done
  for part in "${stack[@]}"; do
    out+="$part/"
  done
  printf '%s\n' "${out%/}"
}

# Emit one candidate link target per line from a Markdown file: inline links,
# images, and reference-style definitions. Fenced code blocks are stripped so
# examples are never mistaken for navigation.
needle_extract_targets() {
  awk '
    /^[[:space:]]*(```|~~~)/ {
      infence = !infence
      next
    }
    infence { next }
    {
      line = $0
      while (match(line, /\[[^]]*\]\(/)) {
        start = RSTART + RLENGTH
        end = start
        depth = 0
        in_angle = 0
        while (end <= length(line)) {
          character = substr(line, end, 1)
          if (character == "<") {
            in_angle = 1
          } else if (character == ">") {
            in_angle = 0
          } else if (!in_angle && character == "(") {
            depth++
          } else if (!in_angle && character == ")") {
            if (depth == 0) {
              break
            }
            depth--
          }
          end++
        }
        if (end > length(line)) {
          break
        }
        print substr(line, start, end - start)
        line = substr(line, end + 1)
      }
    }
    /^[[:space:]]*\[[^]]+\]:/ {
      rest = $0
      sub(/^[^]]*\]:[[:space:]]*/, "", rest)
      print rest
    }
  ' "$1"
}

# Is this target one the checker must resolve? Pure anchors and schemed URLs
# (https:, mailto:, ...) are outside the contract.
needle_target_is_internal() {
  local target="$1"
  [[ -z "$target" || "$target" == \#* ]] && return 1
  [[ "$target" == //* ]] && return 1
  [[ "$target" =~ ^[a-zA-Z][a-zA-Z0-9+.-]*: ]] && return 1
  return 0
}

# Extract the destination from a Markdown destination, ignoring an optional
# title. Angle-bracket destinations may contain spaces; unbracketed
# destinations end at the first whitespace.
needle_target_path() {
  local target="$1"
  target="${target#"${target%%[![:space:]]*}"}"
  target="${target%"${target##*[![:space:]]}"}"
  if [[ "$target" == "<"* ]]; then
    [[ "$target" == *">"* ]] || return 1
    target="${target#<}"
    target="${target%%>*}"
  else
    target="${target%%[[:space:]]*}"
  fi
  printf '%s\n' "$target"
}

declare -a INDEX_TARGETS=()
index_links_validated=0
links_checked=0
files_checked=0

# A developer checkout can contain ignored analysis artifacts that will not be
# present in CI's clean archive. Include tracked and nonignored untracked
# Markdown there, while keeping the archive path independent of Git.
needle_documentation_files() {
  local git_root relative
  if git_root="$(git -C "$DOC_ROOT" rev-parse --show-toplevel 2>/dev/null)" &&
    [[ "$git_root" == "$DOC_ROOT" ]]; then
    while IFS= read -r -d '' relative; do
      case "$relative" in
        README.md | docs/*.md) printf '%s\0' "$DOC_ROOT/$relative" ;;
      esac
    done < <(git -C "$DOC_ROOT" ls-files -z --cached --others --exclude-standard -- docs README.md)
    return 0
  fi

  find "$DOC_ROOT/docs" -type f -name '*.md' -print0
  [[ -f "$DOC_ROOT/README.md" ]] && printf '%s\0' "$DOC_ROOT/README.md"
}

# Validate every internal link in one Markdown file. Index targets are also
# collected (with fragments kept) for the duplicate check below.
needle_check_file() {
  local file="$1" relative="$2" dir raw target path resolved
  dir="$(needle_normalize_path "$(dirname "$file")" "")"

  while IFS= read -r raw; do
    [[ -n "$raw" ]] || continue
    target="$(needle_target_path "$raw")" || {
      record "malformed internal link destination: $relative -> $raw"
      continue
    }
    needle_target_is_internal "$target" || continue

    if [[ "$file" == "$INDEX" ]]; then
      INDEX_TARGETS+=("$target")
    fi

    path="${target%%#*}"
    path="${path%%\?*}"
    [[ -n "$path" ]] || continue

    links_checked=$((links_checked + 1))
    if [[ "$path" == /* ]]; then
      resolved="$(needle_normalize_path "$DOC_ROOT" "${path#/}")"
    else
      resolved="$(needle_normalize_path "$dir" "$path")"
    fi
    if [[ "$resolved" != "$DOC_ROOT" && "$resolved" != "$DOC_ROOT"/* ]]; then
      record "link escapes repository root: $relative -> $target"
    elif [[ ! -e "$resolved" ]]; then
      record "broken internal link: $relative -> $target"
    else
      index_links_validated=$((index_links_validated + 1))
    fi
  done < <(needle_extract_targets "$file")
}

# ── Link resolution ──────────────────────────────────────────────────────────
if [[ ! -d "$DOC_ROOT/docs" ]]; then
  echo "Error: $DOC_ROOT/docs is not a directory" >&2
  exit 1
fi

while IFS= read -r -d '' file; do
  files_checked=$((files_checked + 1))
  needle_check_file "$file" "${file#"$DOC_ROOT"/}"
done < <(needle_documentation_files | sort -z)

# ── Index-only contracts ─────────────────────────────────────────────────────
if [[ ! -f "$INDEX" ]]; then
  record "documentation index missing: docs/README.md"
else
  # 1. No target may be linked twice — duplicate rows are stale navigation.
  if (( ${#INDEX_TARGETS[@]} > 0 )); then
    while IFS= read -r duplicate; do
      [[ -n "$duplicate" ]] || continue
      record "index links the same target more than once: $duplicate"
    done < <(printf '%s\n' ${INDEX_TARGETS[@]+"${INDEX_TARGETS[@]}"} | sort | uniq -d)
  fi

  # 2. Curated directories must be fully indexed.
  if [[ -d "$DOC_ROOT/docs/adr" ]]; then
    while IFS= read -r -d '' adr; do
      grep -Fq "adr/$(basename "$adr")" "$INDEX" ||
        record "ADR not indexed in docs/README.md: adr/$(basename "$adr")"
    done < <(find "$DOC_ROOT/docs/adr" -maxdepth 1 -type f -name '*.md' -print0 | sort -z)
  fi
  if [[ -d "$DOC_ROOT/docs/research" ]]; then
    while IFS= read -r -d '' doc; do
      grep -Fq "research/$(basename "$doc")" "$INDEX" ||
        record "research document not indexed in docs/README.md: research/$(basename "$doc")"
    done < <(find "$DOC_ROOT/docs/research" -maxdepth 1 -type f -name '*.md' -print0 | sort -z)
  fi

  # 3. Documented inventory counts must match the tree.
  needle_actual_count() {
    local root="$1" file count=0
    while IFS= read -r -d '' file; do
      if [[ "$file" == "$root"/* ]]; then
        count=$((count + 1))
      fi
    done < <(needle_documentation_files)
    printf '%s\n' "$count"
  }

  # Compare one inventory statement against reality. The label must appear
  # exactly once; a missing statement fails so the summary cannot be deleted
  # to silence the check.
  needle_check_inventory() {
    local label="$1" pattern="$2" actual="$3" matches documented
    matches="$(grep -cE "$pattern" "$INDEX" || true)"
    if [[ "$matches" -eq 0 ]]; then
      record "index is missing the required inventory statement: $label"
      return
    fi
    if [[ "$matches" -gt 1 ]]; then
      record "index states $label more than once ($matches occurrences)"
      return
    fi
    documented="$(grep -oE "$pattern" "$INDEX" | grep -oE '[0-9]+' | head -1)"
    if [[ "$documented" != "$actual" ]]; then
      record "inventory mismatch: $label documented $documented, actual $actual"
    fi
  }

  needle_check_inventory \
    "Total markdown files (index prose)" \
    'contains [0-9]+ markdown files' \
    "$(needle_actual_count "$DOC_ROOT/docs")"
  needle_check_inventory \
    "Total markdown files" \
    'Total markdown files:\*{0,2} [0-9]+' \
    "$(needle_actual_count "$DOC_ROOT/docs")"
  needle_check_inventory \
    "ADRs" \
    'ADRs:\*{0,2} [0-9]+' \
    "$(needle_actual_count "$DOC_ROOT/docs/adr")"
  needle_check_inventory \
    "Research documents" \
    'Research documents:\*{0,2} [0-9]+' \
    "$(needle_actual_count "$DOC_ROOT/docs/research")"
  needle_check_inventory \
    "Operational notes" \
    'Operational notes:\*{0,2} [0-9]+' \
    "$(needle_actual_count "$DOC_ROOT/docs/notes")"
fi

# ── Report ───────────────────────────────────────────────────────────────────
if (( ${#FAILURES[@]} > 0 )); then
  echo "documentation navigation check: ${#FAILURES[@]} failure(s):" >&2
  for failure in "${FAILURES[@]}"; do
    echo "  - $failure" >&2
  done
  echo "See docs/ci/doc-navigation-check.md for the contract and how to fix." >&2
  exit 1
fi

echo "documentation navigation check: $files_checked files, $links_checked internal links, inventory verified"
