#!/usr/bin/env bash
# NEEDLE verification gate: a dispatch that bypassed the Definition-of-Done
# pre-commit hook (`git commit --no-verify`) is a FAILED dispatch, whatever the
# commit contains. The hook records every bypass in .beads/bypasses.jsonl with
# the commit sha; this gate fails when any commit that names the bead being
# closed appears there. Runs in the shared workspace (run_in: workspace) so it
# sees the live, untracked-by-CI bypass log.
#
# Env from NEEDLE: NEEDLE_BEAD_ID, NEEDLE_WORKSPACE (issue #7).
set -uo pipefail
ws="${NEEDLE_WORKSPACE:-$PWD}"
bead="${NEEDLE_BEAD_ID:-}"
log="$ws/.beads/bypasses.jsonl"
[ -n "$bead" ] || { echo "gate-no-dod-bypass: NEEDLE_BEAD_ID not set — nothing to check"; exit 0; }
[ -f "$log" ] || exit 0
mapfile -t shas < <(git -C "$ws" log --format=%H -n 200 --grep="$bead" 2>/dev/null)
[ ${#shas[@]} -gt 0 ] || exit 0
bad=()
for sha in "${shas[@]}"; do
  if grep -q "\"commit_sha\":\"$sha\"" "$log"; then bad+=("$sha"); fi
done
if [ ${#bad[@]} -gt 0 ]; then
  echo "gate-no-dod-bypass: bead $bead has commit(s) that bypassed the Definition of Done:"
  for sha in "${bad[@]}"; do echo "  $sha  $(git -C "$ws" log -1 --format=%s "$sha" | cut -c1-90)"; done
  echo "A --no-verify commit is a failed dispatch. Re-run scripts/definition-of-done.sh --fast, fix what it reports, and commit again with the hook enabled."
  exit 1
fi
echo "gate-no-dod-bypass: no bypassed commits for $bead"
