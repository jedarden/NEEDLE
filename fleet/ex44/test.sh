#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SRC_DIR/workers.tsv"

bash -n "$SRC_DIR/apply-ex44-fleet.sh" "$SRC_DIR/backlog-slo.sh" "$SRC_DIR/needle-zai-governor" "$SRC_DIR/needle-release-upgrade"
! grep -q 'required-explore-workspaces' "$SRC_DIR/apply-ex44-fleet.sh" "$SRC_DIR/README.md"
! grep -q 'explicit Explore list' "$SRC_DIR/apply-ex44-fleet.sh"
grep -q 'leaves Explore.*workspace list empty' "$SRC_DIR/README.md"
grep -q 'deliberate pinning exception' "$SRC_DIR/README.md"

rows=$(awk -F'\t' '$1 !~ /^#/ && NF == 5 {print}' "$MANIFEST")
[[ "$(wc -l <<<"$rows")" -eq 32 ]]
[[ "$(cut -f1 <<<"$rows" | sort -u | wc -l)" -eq 32 ]]
[[ "$(awk -F'\t' '$5 == "true" {n++} END {print n+0}' <<<"$rows")" -eq 27 ]]
grep -q $'^codex-luna-tradegraph\t/home/coding/.needle/roam-only\t.*\ttrue$' "$MANIFEST"
grep -q $'^codex-luna-adc\t/home/coding/.needle/roam-only\t.*\ttrue$' "$MANIFEST"
[[ "$(awk -F'\t' '$3 == "codex-gpt-5.6-luna-xhigh" {n++} END {print n+0}' <<<"$rows")" -eq 8 ]]
[[ "$(awk -F'\t' '$1 ~ /^codex-luna-tgplat-0[12]$/ && $2 == "/home/coding/tradegraph-platform" && $5 == "false" {n++} END {print n+0}' <<<"$rows")" -eq 2 ]]
[[ "$(awk -F'\t' '$3 == "codex-gpt-6-luna-xhigh" {n++} END {print n+0}' <<<"$rows")" -eq 1 ]]
grep -q $'^codex-needle-01\t/home/coding/NEEDLE\tcodex-gpt-5.6-luna-xhigh\t0\ttrue$' "$MANIFEST"
grep -q $'^codex-luna-tradegraph\t/home/coding/.needle/roam-only\tcodex-gpt-5.6-luna-xhigh\t90\ttrue$' "$MANIFEST"
grep -q $'^codex-luna-adc\t/home/coding/.needle/roam-only\tcodex-gpt-5.6-luna-xhigh\t135\ttrue$' "$MANIFEST"
grep -q $'^codex-luna-needle-01\t/home/coding/NEEDLE\tcodex-gpt-6-luna-xhigh\t315\tfalse$' "$MANIFEST"
grep -q $'^codex-luna-warp\t/home/coding/WARP\tcodex-gpt-5.6-luna-xhigh\t45\tfalse$' "$MANIFEST"
! grep -Eq $'^(glm-tradegraph|glm53-adc|glm-needle-01)\t' "$MANIFEST"
grep -q 'NEEDLE_AGENT__EVIDENCE_ROUTING__ENABLED=false' "$SRC_DIR/apply-ex44-fleet.sh"
grep -q '^  backend: bead-rs$' "$SRC_DIR/roam-home.yaml"
[[ "$(awk -F'\t' '$3 == "claude-code-glm-5.3" {n++} END {print n+0}' <<<"$rows")" -le 9 ]]
! grep -Eq '/(CLASP|agentists-quickstart-deprecated|commitgraph-deprecated)([[:space:]]|$)' "$MANIFEST"

grep -qx 'NEEDLE_STRANDS__GENERATION__LOW_WATER_RESERVE=6' "$SRC_DIR/fleet-policy.env"
! grep -q 'NEEDLE_STRANDS__GENERATION__ENABLED' "$SRC_DIR/apply-ex44-fleet.sh"
grep -qx 'NEEDLE_STRANDS__MITOSIS__TIMEOUT_TRIGGERED__AGENT_WALLCLOCK_TIMEOUT=true' "$SRC_DIR/fleet-policy.env"
grep -qx 'NEEDLE_WORKER__MAX_WORKERS=32' "$SRC_DIR/fleet-policy.env"
grep -qx 'FLEET_WORKER_TARGET=27' "$SRC_DIR/backlog-policy.env"
grep -qx 'FLEET_ELIGIBLE_TARGET=108' "$SRC_DIR/backlog-policy.env"
grep -qx 'FLEET_ELIGIBLE_MINIMUM=54' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_TARGET_PER_WORKER=4' "$SRC_DIR/backlog-policy.env"
grep -qx 'ELIGIBLE_MINIMUM_PER_WORKER=2' "$SRC_DIR/backlog-policy.env"
grep -qx 'provider: openai' "$SRC_DIR/adapters/codex-gpt-5.6-luna-xhigh.yaml"
grep -q -- 'codex exec --json --model gpt-5.6-luna' "$SRC_DIR/adapters/codex-gpt-5.6-luna-xhigh.yaml"
grep -qx 'output_transform: needle-transform-codex' "$SRC_DIR/adapters/codex-gpt-5.6-luna-xhigh.yaml"
grep -qx 'provider: openai' "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml"
grep -qx 'model: gpt-6-luna' "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml"
grep -q -- 'codex exec --json --model gpt-6-luna' "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml"
grep -qx 'usage_format: codex_jsonl' "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml"
grep -qx 'output_transform: needle-transform-codex' "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml"
cmp -s "$SRC_DIR/adapters/codex-gpt-5.6-luna-xhigh.yaml" \
    <(sed -e 's/gpt-6-luna/gpt-5.6-luna/g' -e 's/GPT-6 Luna/GPT-5.6 Luna/g' \
        "$SRC_DIR/adapters/codex-gpt-6-luna-xhigh.yaml")
grep -q 'Environment=PATH=.*/home/coding/.local/bin' "$SRC_DIR/needle-backlog-slo.service"
grep -qx 'ELASTIC_UNITS=(glm-icg glm-roam-18 glm-roam-19 glm-roam-20 glm-roam-21 glm-roam-22 glm-roam-23 glm-roam-24)' "$SRC_DIR/needle-zai-governor"
grep -qx 'TOLERATED_AFFECTED_REQUESTS=1' "$SRC_DIR/needle-zai-governor"
grep -q 'stop_is_safe "$state" "$bead_id"' "$SRC_DIR/needle-zai-governor"

fleet_policy_line=$(grep -n 'EnvironmentFile=%h/.config/needle/fleet-policy.env' "$SRC_DIR/needle-worker@.service" | cut -d: -f1)
instance_policy_line=$(grep -n 'EnvironmentFile=%h/.config/needle/workers/%i.env' "$SRC_DIR/needle-worker@.service" | cut -d: -f1)
[[ "$fleet_policy_line" -lt "$instance_policy_line" ]]

# The audit runs once a day, reports rather than repairs, and treats findings
# (exit 1) as a verdict instead of a broken probe.
grep -q 'ExecStart=/home/coding/.needle/bin/needle audit --emit-telemetry --file-beads --json' "$SRC_DIR/needle-factory-audit.service"
grep -qx 'SuccessExitStatus=1' "$SRC_DIR/needle-factory-audit.service"
grep -q '^OnCalendar=\*-\*-\* ' "$SRC_DIR/needle-factory-audit.timer"
grep -qx 'Persistent=true' "$SRC_DIR/needle-factory-audit.timer"
grep -q 'needle-factory-audit.timer' "$SRC_DIR/apply-ex44-fleet.sh"

# The ADR-029 improvement loop (needle-7c064803) also runs once a day, as a
# one-shot CLI with no background strand of its own -- same external-scheduler
# shape as the factory audit, deliberately offset ahead of it.
grep -q 'ExecStart=/home/coding/.needle/bin/needle improve --json' "$SRC_DIR/needle-improve.service"
grep -q '^OnCalendar=\*-\*-\* ' "$SRC_DIR/needle-improve.timer"
grep -qx 'Persistent=true' "$SRC_DIR/needle-improve.timer"
grep -q 'needle-improve.timer' "$SRC_DIR/apply-ex44-fleet.sh"

# The standalone fleet has no `needle supervise` daemon, so release discovery
# must be host-level while still using the canary-gated upgrade path.
grep -q 'ExecStart=/run/current-system/sw/bin/flock --nonblock %h/.needle/state/release-upgrade.lock /home/coding/.local/bin/needle-release-upgrade' "$SRC_DIR/needle-release-upgrade.service"
grep -qx 'WorkingDirectory=/home/coding/NEEDLE' "$SRC_DIR/needle-release-upgrade.service"
grep -qx 'OnUnitActiveSec=6h' "$SRC_DIR/needle-release-upgrade.timer"
grep -qx 'Persistent=true' "$SRC_DIR/needle-release-upgrade.timer"
grep -q 'needle-release-upgrade" "$NEEDLE_HOST_HOME/.local/bin/needle-release-upgrade"' "$SRC_DIR/apply-ex44-fleet.sh"
grep -q 'needle-release-upgrade.service' "$SRC_DIR/apply-ex44-fleet.sh"
grep -q 'needle-release-upgrade.timer' "$SRC_DIR/apply-ex44-fleet.sh"

# The activation fragment keeps the lane, the workspace-only codex candidate
# and the OpenAI cap together: enabling the candidate without the cap is what
# would let one adapter pull the fleet onto an unbilled-by-us provider.
grep -q 'workers: \[codex-needle-01, codex-luna-needle-01, claude-needle-01\]' "$SRC_DIR/bootstrap-lane.yaml"
grep -q 'workspace_only_candidates:' "$SRC_DIR/bootstrap-lane.yaml"
grep -q 'openai:' "$SRC_DIR/bootstrap-lane.yaml"

"$SRC_DIR/backlog-slo.sh" --self-test
"$SRC_DIR/needle-zai-governor" --self-test
"$SRC_DIR/needle-release-upgrade" --self-test
echo "ex44 fleet policy tests passed"
