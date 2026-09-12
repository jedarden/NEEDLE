#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SRC_DIR/workers.tsv"

bash -n "$SRC_DIR/apply-ex44-fleet.sh" "$SRC_DIR/backlog-slo.sh"
[[ -r "$SRC_DIR/required-explore-workspaces.txt" ]]

rows=$(awk -F'\t' '$1 !~ /^#/ && NF == 5 {print}' "$MANIFEST")
[[ "$(wc -l <<<"$rows")" -eq 21 ]]
[[ "$(cut -f1 <<<"$rows" | sort -u | wc -l)" -eq 21 ]]
[[ "$(awk -F'\t' '$5 == "true" {n++} END {print n+0}' <<<"$rows")" -eq 3 ]]
[[ "$(awk -F'\t' '$3 == "claude-code-glm-5.3" {n++} END {print n+0}' <<<"$rows")" -le 9 ]]
! grep -Eq '/(CLASP|agentists-quickstart-deprecated|commitgraph-deprecated)([[:space:]]|$)' "$MANIFEST"
grep -qx '/home/coding/FABRIC' "$SRC_DIR/required-explore-workspaces.txt"

grep -qx 'NEEDLE_STRANDS__GENERATION__LOW_WATER_RESERVE=6' "$SRC_DIR/fleet-policy.env"
grep -qx 'NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__AGENT_WALLCLOCK_TIMEOUT=true' "$SRC_DIR/fleet-policy.env"
grep -qx 'FLEET_ELIGIBLE_TARGET=84' "$SRC_DIR/backlog-policy.env"
grep -qx 'FLEET_ELIGIBLE_MINIMUM=42' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_TARGET_PER_WORKER=4' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_MINIMUM_PER_WORKER=2' "$SRC_DIR/backlog-policy.env"

"$SRC_DIR/backlog-slo.sh" --self-test
echo "ex44 fleet policy tests passed"
