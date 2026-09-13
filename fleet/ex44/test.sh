#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SRC_DIR/workers.tsv"

bash -n "$SRC_DIR/apply-ex44-fleet.sh" "$SRC_DIR/backlog-slo.sh" "$SRC_DIR/needle-zai-governor"
[[ -r "$SRC_DIR/required-explore-workspaces.txt" ]]

rows=$(awk -F'\t' '$1 !~ /^#/ && NF == 5 {print}' "$MANIFEST")
[[ "$(wc -l <<<"$rows")" -eq 25 ]]
[[ "$(cut -f1 <<<"$rows" | sort -u | wc -l)" -eq 25 ]]
[[ "$(awk -F'\t' '$5 == "true" {n++} END {print n+0}' <<<"$rows")" -eq 7 ]]
[[ "$(awk -F'\t' '$3 == "claude-code-glm-5.3" {n++} END {print n+0}' <<<"$rows")" -le 9 ]]
! grep -Eq '/(CLASP|agentists-quickstart-deprecated|commitgraph-deprecated)([[:space:]]|$)' "$MANIFEST"
grep -qx '/home/coding/FABRIC' "$SRC_DIR/required-explore-workspaces.txt"

grep -qx 'NEEDLE_STRANDS__GENERATION__LOW_WATER_RESERVE=6' "$SRC_DIR/fleet-policy.env"
grep -qx 'NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__AGENT_WALLCLOCK_TIMEOUT=true' "$SRC_DIR/fleet-policy.env"
grep -qx 'NEEDLE_WORKER__MAX_WORKERS=25' "$SRC_DIR/fleet-policy.env"
grep -qx 'FLEET_ELIGIBLE_TARGET=100' "$SRC_DIR/backlog-policy.env"
grep -qx 'FLEET_ELIGIBLE_MINIMUM=50' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_TARGET_PER_WORKER=4' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_MINIMUM_PER_WORKER=2' "$SRC_DIR/backlog-policy.env"
grep -q 'Environment=PATH=.*/home/coding/.local/bin' "$SRC_DIR/needle-backlog-slo.service"
grep -qx 'ELASTIC_UNITS=(glm-icg glm-roam-18 glm-roam-19 glm-roam-20 glm-roam-21 glm-roam-22 glm-roam-23 glm-roam-24)' "$SRC_DIR/needle-zai-governor"
grep -qx 'TOLERATED_AFFECTED_REQUESTS=1' "$SRC_DIR/needle-zai-governor"

fleet_policy_line=$(grep -n 'EnvironmentFile=%h/.config/needle/fleet-policy.env' "$SRC_DIR/needle-worker@.service" | cut -d: -f1)
instance_policy_line=$(grep -n 'EnvironmentFile=%h/.config/needle/workers/%i.env' "$SRC_DIR/needle-worker@.service" | cut -d: -f1)
[[ "$fleet_policy_line" -lt "$instance_policy_line" ]]

"$SRC_DIR/backlog-slo.sh" --self-test
"$SRC_DIR/needle-zai-governor" --self-test
echo "ex44 fleet policy tests passed"
