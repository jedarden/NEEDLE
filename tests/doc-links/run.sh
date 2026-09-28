#!/usr/bin/env bash
# Mutation tests for scripts/check-doc-links.sh.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHECKER="$REPO_ROOT/scripts/check-doc-links.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-doc-links-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

passes=0
failures=0

ok() {
  echo "PASS: $1"
  passes=$((passes + 1))
}

bad() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

# A minimal healthy documentation tree. The index prose and the summary both
# state the real count (7 markdown files under docs/), every curated document
# is indexed exactly once, and page.md carries the shapes that must NOT trip
# the checker: an external URL, a pure anchor, a fenced code block containing
# a fake link, and a reference-style definition.
fresh_fixture() {
  local destination="$1"
  rm -rf "$destination"
  mkdir -p "$destination/docs/adr" "$destination/docs/research" \
    "$destination/docs/notes" "$destination/docs/examples/quickstart"

  cat >"$destination/README.md" <<'EOF'
# Fixture

Read the [documentation index](docs/README.md), visit the
[project site](https://example.com/needle), or jump to [the top](#fixture).
EOF

  cat >"$destination/docs/README.md" <<'EOF'
# Fixture Documentation Index

This directory contains 7 markdown files.

| Document |
|----------|
| [Overview](page.md) |
| [Quickstart](examples/quickstart/README.md) |
| [ADR-001](adr/001-fixture.md) |
| [Research](research/r1.md) |

## File Count Summary

- **Total markdown files:** 7
- **ADRs:** 1
- **Research documents:** 1
- **Operational notes:** 2
EOF

  cat >"$destination/docs/page.md" <<'EOF'
# Overview

Self-reference with a fragment: [section](page.md#section).
Reference definition: [the note][note-ref].
Angle-bracket destination with a title: [quickstart](<examples/quickstart/README.md> "title").
Nested parentheses in a destination: [draft](<notes/v1 (draft).md>).
Root-relative destination: [this page](/docs/page.md).

[note-ref]: notes/n1.md

```
Do not resolve [this](missing-inside-fence.md); it is an example.
```
EOF

  printf '# ADR-001\n' >"$destination/docs/adr/001-fixture.md"
  printf '# Research\n' >"$destination/docs/research/r1.md"
  printf '# Note\n' >"$destination/docs/notes/n1.md"
  printf '# Draft note\n' >"$destination/docs/notes/v1 (draft).md"
  printf '# Quickstart\n' >"$destination/docs/examples/quickstart/README.md"
}

# Run the checker against a fixture root; echoes output, returns its status.
run_checker() {
  local root="$1" output status
  if output="$(NEEDLE_DOCUMENTATION_ROOT="$root" bash "$CHECKER" 2>&1)"; then
    status=0
  else
    status=1
  fi
  printf '%s\n' "$output"
  return "$status"
}

expect_success() {
  local name="$1" root="$2" output
  if output="$(run_checker "$root")"; then
    ok "$name"
  else
    bad "$name: checker rejected a healthy tree: $output"
  fi
}

expect_failure() {
  local name="$1" expected="$2" root="$3" output
  if output="$(run_checker "$root")"; then
    bad "$name was accepted"
  elif grep -Fq -- "$expected" <<<"$output"; then
    ok "$name"
  else
    bad "$name failed without '$expected': $output"
  fi
}

healthy="$TMP_ROOT/healthy"
fresh_fixture "$healthy"
expect_success "healthy tree passes" "$healthy"

case_root="$TMP_ROOT/broken-link"
fresh_fixture "$case_root"
printf '\nSee [the glossary](glossary.md).\n' >>"$case_root/docs/page.md"
expect_failure "broken internal link is rejected" \
  "broken internal link: docs/page.md -> glossary.md" "$case_root"

case_root="$TMP_ROOT/escaping-link"
fresh_fixture "$case_root"
printf '\nSee [outside](../../elsewhere.md).\n' >>"$case_root/docs/page.md"
expect_failure "link escaping the repository root is rejected" \
  "link escapes repository root: docs/page.md -> ../../elsewhere.md" "$case_root"

case_root="$TMP_ROOT/root-readme-link"
fresh_fixture "$case_root"
printf '\nSee [docs](nope.md).\n' >>"$case_root/README.md"
expect_failure "broken link in the root README is rejected" \
  "broken internal link: README.md -> nope.md" "$case_root"

case_root="$TMP_ROOT/stale-total"
fresh_fixture "$case_root"
sed -i 's/contains 7 markdown files/contains 6 markdown files/' \
  "$case_root/docs/README.md"
expect_failure "stale inventory count is rejected" \
  "documented 6, actual 7" "$case_root"

case_root="$TMP_ROOT/stale-summary-total"
fresh_fixture "$case_root"
sed -i 's/\*\*Total markdown files:\*\* 7/**Total markdown files:** 8/' \
  "$case_root/docs/README.md"
expect_failure "stale summary count is rejected" \
  "documented 8, actual 7" "$case_root"

case_root="$TMP_ROOT/missing-inventory-statement"
fresh_fixture "$case_root"
sed -i '/Operational notes:/d' "$case_root/docs/README.md"
expect_failure "deleted inventory statement is rejected" \
  "missing the required inventory statement: Operational notes" "$case_root"

case_root="$TMP_ROOT/unindexed-adrs"
fresh_fixture "$case_root"
cp "$case_root/docs/adr/001-fixture.md" "$case_root/docs/adr/002-new.md"
sed -i -e 's/contains 7 markdown files/contains 8 markdown files/' \
  -e 's/\*\*Total markdown files:\*\* 7/**Total markdown files:** 8/' \
  -e 's/\*\*ADRs:\*\* 1/**ADRs:** 2/' \
  "$case_root/docs/README.md"
expect_failure "ADR missing from the index is rejected" \
  "ADR not indexed in docs/README.md: adr/002-new.md" "$case_root"

case_root="$TMP_ROOT/unindexed-research"
fresh_fixture "$case_root"
cp "$case_root/docs/research/r1.md" "$case_root/docs/research/r2.md"
sed -i -e 's/contains 7 markdown files/contains 8 markdown files/' \
  -e 's/\*\*Total markdown files:\*\* 7/**Total markdown files:** 8/' \
  -e 's/\*\*Research documents:\*\* 1/**Research documents:** 2/' \
  "$case_root/docs/README.md"
expect_failure "research document missing from the index is rejected" \
  "research document not indexed in docs/README.md: research/r2.md" "$case_root"

case_root="$TMP_ROOT/duplicate-index-target"
fresh_fixture "$case_root"
sed -i 's@| \[Overview\](page.md) |@| [Overview](page.md) |\n| [Overview again](page.md) |@' \
  "$case_root/docs/README.md"
expect_failure "duplicate index target is rejected" \
  "index links the same target more than once: page.md" "$case_root"

case_root="$TMP_ROOT/missing-index"
fresh_fixture "$case_root"
rm "$case_root/docs/README.md"
expect_failure "missing documentation index is rejected" \
  "documentation index missing: docs/README.md" "$case_root"

echo "doc-links checker tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
