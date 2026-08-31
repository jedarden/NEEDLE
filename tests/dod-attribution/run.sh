#!/usr/bin/env bash
# Tests for the Definition of Done's failure attribution (--changed-only).
#
# This is the logic that decides whether a failing fast lane blocks the commit
# or is reported as somebody else's in-flight breakage. Getting it wrong in the
# permissive direction lets bad commits through; getting it wrong in the strict
# direction is what drove 607 recorded --no-verify bypasses. So it is tested.
#
# The functions are extracted from the real script rather than copied, so this
# cannot drift away from what actually runs.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DOD="$REPO_ROOT/scripts/definition-of-done.sh"
[[ -f "$DOD" ]] || { echo "missing $DOD" >&2; exit 1; }

extracted="$(mktemp "${TMPDIR:-/tmp}/dod-attr-XXXXXX.sh")"
for fn in needle_path_is_staged needle_diagnostic_paths needle_failure_is_ours; do
  awk -v f="^${fn}\\\\(\\\\)" '$0 ~ f, /^}/' "$DOD" >> "$extracted"
done
# Every function must have been found, or the test would silently pass.
for fn in needle_path_is_staged needle_diagnostic_paths needle_failure_is_ours; do
  grep -q "^${fn}()" "$extracted" || { echo "FAIL: could not extract $fn from $DOD" >&2; exit 1; }
done
# shellcheck source=/dev/null
source "$extracted"

log="$(mktemp "${TMPDIR:-/tmp}/dod-attr-log-XXXXXX")"
pass=0
fail=0

check() {
  local desc="$1" want="$2" got
  needle_failure_is_ours "$log"
  got=$?
  if [[ "$got" == "$want" ]]; then
    pass=$((pass + 1))
    echo "  ok   $desc"
  else
    fail=$((fail + 1))
    echo "  FAIL $desc (expected $want, got $got)"
  fi
}

echo "=== Definition of Done attribution ==="

CHANGED_ONLY=true
STAGED_PATHS=("src/worker/mod.rs" "scripts/definition-of-done.sh")

printf 'src/monitoring/mod.rs:62:41: error[E0382]: borrow of moved value\nerror: could not compile `needle`\n' > "$log"
check "diagnostics only in unstaged files do not block" 1

printf 'src/worker/mod.rs:120:9: error[E0425]: cannot find value\n' > "$log"
check "a diagnostic in a staged file blocks" 0

printf 'src/monitoring/mod.rs:62:41: error[E0382]: x\nsrc/worker/mod.rs:9:1: warning: unused import\n' > "$log"
check "mixed diagnostics block when one is staged" 0

printf 'src/monitoring/repair.rs:160:46: warning: unused variable\n' > "$log"
check "a warning in an unstaged file does not block" 1

printf 'error: linking with `cc` failed\nerror: could not compile `needle`\n' > "$log"
check "an unattributable failure blocks (conservative)" 0

: > "$log"
check "no diagnostics at all blocks (conservative)" 0

printf './src/worker/mod.rs:1:1: error[E0001]: x\n' > "$log"
check "a leading ./ still matches a staged path" 0

CHANGED_ONLY=false
printf 'src/monitoring/mod.rs:62:41: error[E0382]: x\n' > "$log"
check "attribution off blocks on any failure" 0

rm -f "$log" "$extracted"
echo "passed=$pass failed=$fail"
[[ "$fail" -eq 0 ]]
