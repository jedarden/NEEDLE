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
# Bypass accounting is part of the same pre-commit contract: the warning must
# count real bypass events, not legacy rows that predate the commit SHA field.
# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/bypass-detection.sh"

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

# ── bypass counting ──────────────────────────────────────────────────────────
# The log also contains legacy rows without commit_sha. They are historical
# telemetry, not bypasses, and must not inflate either count in the warning.
bypass_log="$(mktemp "${TMPDIR:-/tmp}/dod-bypass-log-XXXXXX")"
today="$(date -u +%Y-%m-%d)"
cat > "$bypass_log" <<EOF
{"timestamp":"${today}T00:00:00Z","lane":"fast","pwd":"/legacy"}
{"timestamp":"${today}T00:01:00Z","commit_sha":"today-sha","lanes_skipped":["fast"]}
{"timestamp":"2020-01-01T00:00:00Z","commit_sha":"old-sha","lanes_skipped":["fast"]}
EOF

counts="$(NEEDLE_BYPASS_LOG="$bypass_log" needle_bypass_counts)"
if [[ "$counts" == "2 1" ]]; then
  pass=$((pass + 1))
  echo "  ok   bypass counts exclude legacy rows and separate total from today"
else
  fail=$((fail + 1))
  echo "  FAIL bypass counts drifted (expected '2 1', got '$counts')"
fi

warning="$(NEEDLE_BYPASS_LOG="$bypass_log" needle_warn_bypass --no-verify fast 2>&1)"
if grep -Fq 'Recorded so far: 2 total, 1 today.' <<<"$warning"; then
  pass=$((pass + 1))
  echo "  ok   bypass warning reports the counted events"
else
  fail=$((fail + 1))
  echo "  FAIL bypass warning omitted the counted events"
fi

rm -f "$log" "$extracted" "$bypass_log"
echo "passed=$pass failed=$fail"
[[ "$fail" -eq 0 ]]
