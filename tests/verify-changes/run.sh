#!/usr/bin/env bash
# Exercise the deployed change-evidence compatibility gate
# (scripts/verify-changes.sh) against isolated, real Git repos.
#
# Bash rather than Python: this suite runs in the definition-of-done fast
# lane, which also runs inside the needle-ci builder image, and that image has
# no python3. The earlier unittest version exited 127 there on every run from
# 2026-09-29 (needle-f20186a8), failing verify-fast before any shard could
# start. Each case below maps one-to-one to a case of that suite.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$(cd "$REPO_ROOT" && realpath "${VERIFY_CHANGES_SCRIPT:-scripts/verify-changes.sh}")"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-change-gate-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

# The pre-commit gate runs with a captured index/worktree in GIT_*. Fixture
# commands must never inherit those or the operator's config.
while IFS='=' read -r name _; do
  case "$name" in GIT_*) unset "$name" ;; esac
done < <(env)
export HOME="$TMP_ROOT/home" XDG_CONFIG_HOME="$TMP_ROOT/config"
mkdir -p "$HOME" "$XDG_CONFIG_HOME"

passes=0
failures=0
case_dir=""

ok() { echo "PASS: $1"; passes=$((passes + 1)); }
bad() { echo "FAIL: $1" >&2; failures=$((failures + 1)); }

# A fresh repo with one commit and the pre-dispatch marker at that commit.
fresh_repo() {
  case_dir="$TMP_ROOT/$1/repo"
  mkdir -p "$case_dir"
  git -C "$case_dir" init -q -b main
  git -C "$case_dir" config user.name Fixture
  git -C "$case_dir" config user.email fixture@example.invalid
  printf 'before\n' > "$case_dir/source.txt"
  git -C "$case_dir" add -- source.txt
  git -C "$case_dir" commit -q -m initial
  git -C "$case_dir" rev-parse HEAD > "$case_dir/.needle-predispatch-sha"
}

gate() {
  local rc=0
  (cd "$case_dir" && timeout 15 bash "$SCRIPT") || rc=$?
  echo "$rc"
}

expect_gate() {
  local name="$1" expected="$2" actual
  actual="$(gate)"
  if [[ "$actual" == "$expected" ]]; then
    ok "$name"
  else
    bad "$name: expected exit $expected, got $actual"
  fi
}

# test_marker_alone_is_not_work
fresh_repo marker-alone
expect_gate "marker alone is not work" 1

# test_missing_marker_preserves_legacy_compatibility
fresh_repo missing-marker
rm -f "$case_dir/.needle-predispatch-sha"
expect_gate "missing marker preserves legacy compatibility" 0

# test_committed_work_with_history_larger_than_pipe_buffer: 1500 commits with
# ~300-byte messages, far beyond a pipe buffer, so an early-exiting consumer
# under pipefail would SIGPIPE git. Must pass deterministically (3 runs).
fresh_repo long-history
parent="$(git -C "$case_dir" rev-parse HEAD)"
message="committed work $(printf 'x%.0s' $(seq 1 300))"$'\n'
{
  for i in $(seq 1 1500); do
    printf 'commit refs/heads/main\nmark :%d\n' "$i"
    printf 'committer Fixture <fixture@example.invalid> 1700000000 +0000\n'
    printf 'data %d\n%s' "${#message}" "$message"
    if [[ "$i" -eq 1 ]]; then printf 'from %s\n\n' "$parent"; else printf 'from :%d\n\n' "$((i - 1))"; fi
  done
} | git -C "$case_dir" fast-import --quiet
for run in 1 2 3; do
  expect_gate "committed work with history larger than pipe buffer (run $run)" 0
done

# test_tracked_change_passes_before_and_after_staging
fresh_repo tracked-change
printf 'after\n' > "$case_dir/source.txt"
expect_gate "tracked change passes before staging" 0
git -C "$case_dir" add -- source.txt
expect_gate "tracked change passes after staging" 0

# test_untracked_listing_larger_than_pipe_buffer
fresh_repo untracked-listing
long_name="$(printf 'x%.0s' $(seq 1 160))"
for i in $(seq 0 1499); do
  : > "$case_dir/$(printf '%04d' "$i")-$long_name"
done
expect_gate "untracked listing larger than pipe buffer" 0

echo "verify-changes gate tests: $passes passed, $failures failed"
[[ "$failures" -eq 0 ]]
