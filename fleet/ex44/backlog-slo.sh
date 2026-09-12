#!/usr/bin/env bash
# Read-only fleet eligible-frontier audit. Exit 0 = at/above minimum, 1 = SLO
# breach, 2 = no verdict. JSON is stable enough for journald/OTLP log ingest.
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_HOST_HOME="${NEEDLE_HOST_HOME:-$HOME}"
CONFIG="${NEEDLE_CONFIG:-$NEEDLE_HOST_HOME/.config/needle/config.yaml}"
MANIFEST="${NEEDLE_WORKER_MANIFEST:-$SRC_DIR/workers.tsv}"
POLICY="${NEEDLE_BACKLOG_POLICY:-$SRC_DIR/backlog-policy.env}"
OUTPUT=json
SELF_TEST=0

for arg in "$@"; do
    case "$arg" in
        --json) OUTPUT=json ;;
        --table) OUTPUT=table ;;
        --self-test) SELF_TEST=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# shellcheck disable=SC1090
source "$POLICY"
: "${FLEET_WORKER_TARGET:?missing FLEET_WORKER_TARGET}"
: "${FLEET_ELIGIBLE_TARGET:?missing FLEET_ELIGIBLE_TARGET}"
: "${FLEET_ELIGIBLE_MINIMUM:?missing FLEET_ELIGIBLE_MINIMUM}"
: "${ELIGIBLE_TARGET_PER_WORKER:?missing ELIGIBLE_TARGET_PER_WORKER}"
: "${ELIGIBLE_MINIMUM_PER_WORKER:?missing ELIGIBLE_MINIMUM_PER_WORKER}"
: "${WORKSPACE_ELIGIBLE_RESERVE:?missing WORKSPACE_ELIGIBLE_RESERVE}"

eligible_counts() {
    jq -sc '
      def timestamp:
        sub("^[^:]+:"; "")
        | sub("\\.[0-9]+"; "")
        | sub("[+]00:00$"; "Z")
        | fromdateiso8601? // 0;
      def timed_hold_active:
        ((startswith("deferred:")) or (startswith("quarantine-until:")))
        and ((timestamp) > now);
      def expired_quarantine_marking($bead):
        (($bead.labels // []) | any(. == "deferred"))
        and (($bead.labels // []) | any(startswith("failure-count:")))
        and (($bead.labels // []) | any(startswith("quarantine-until:")))
        and (($bead.labels // [])
             | all(if startswith("quarantine-until:")
                   then (timed_hold_active | not)
                   else true end));
      def hard_label($bead):
        ($bead.labels // [])
        | any(. as $raw
              | ($raw | ascii_downcase) as $label
              | ($label == "blocked"
                 or $label == "manual_blocked"
                 or $label == "manual-blocked"
                 or $label == "blocked:manual"
                 or $label == "human"
                 or ($label | startswith("human:"))
                 or $label == "human-owned"
                 or $label == "owner:human"
                 or (($label | startswith("deferred:")) and ($label | timed_hold_active))
                 or (($label | startswith("quarantine-until:")) and ($label | timed_hold_active))
                 or ($label == "deferred" and (expired_quarantine_marking($bead) | not))));
      map(if type == "array" then .[] else . end) as $beads
      | {
          raw_ready: ($beads | length),
          eligible_ready: ([$beads[] | select(hard_label(.) | not)] | length)
        }
    '
}

if [[ "$SELF_TEST" == 1 ]]; then
    actual=$(
        printf '%s\n' \
            '{"labels":[]}' \
            '{"labels":["human"]}' \
            '{"labels":["deferred","failure-count:1","quarantine-until:2020-01-01T00:00:00Z"]}' \
            '{"labels":["deferred"]}' \
        | eligible_counts
    )
    [[ "$(jq -r '.raw_ready' <<<"$actual")" -eq 4 ]]
    [[ "$(jq -r '.eligible_ready' <<<"$actual")" -eq 2 ]]
    echo "backlog-slo self-test passed"
    exit 0
fi

[[ -r "$CONFIG" ]] || { echo "backlog SLO: config unreadable: $CONFIG" >&2; exit 2; }
[[ -r "$MANIFEST" ]] || { echo "backlog SLO: manifest unreadable: $MANIFEST" >&2; exit 2; }
command -v bead >/dev/null || { echo "backlog SLO: bead is not installed" >&2; exit 2; }
command -v jq >/dev/null || { echo "backlog SLO: jq is not installed" >&2; exit 2; }

rows=$(mktemp "${TMPDIR:-/tmp}/needle-backlog-slo.XXXXXX")
paths=$(mktemp "${TMPDIR:-/tmp}/needle-backlog-paths.XXXXXX")
roam_paths=$(mktemp "${TMPDIR:-/tmp}/needle-backlog-roam.XXXXXX")
pinned_routes=$(mktemp "${TMPDIR:-/tmp}/needle-backlog-routes.XXXXXX")
trap 'rm -f "$rows" "$paths" "$roam_paths" "$pinned_routes"' EXIT

{
    awk '
      /^    workspaces:/ { in_list=1; next }
      in_list && /^      - \/home\/coding\// { print $2; next }
      in_list && !/^    #/ && !/^      - / { exit }
    ' "$CONFIG"
    awk -F'\t' '$1 !~ /^#/ && NF == 5 && $5 == "true" {print $2}' "$MANIFEST"
} | sort -u >"$roam_paths"

{
    cat "$roam_paths"
    awk -F'\t' '$1 !~ /^#/ && NF == 5 {print $2}' "$MANIFEST"
} | sort -u >"$paths"

awk -F'\t' '$1 !~ /^#/ && NF == 5 && $5 == "false" {print $2}' "$MANIFEST" \
    | sort | uniq -c \
    | while read -r workers workspace; do
        jq -cn --arg workspace "$workspace" --argjson workers "$workers" \
            '{workspace:$workspace, workers:$workers}'
    done >"$pinned_routes"

roaming_workers=$(awk -F'\t' '$1 !~ /^#/ && NF == 5 && $5 == "true" {n++} END {print n+0}' "$MANIFEST")

while IFS= read -r workspace; do
    [[ -n "$workspace" ]] || continue
    if [[ ! -f "$workspace/.beads/config.json" ]]; then
        echo "backlog SLO: no bead-rs store at $workspace" >&2
        exit 2
    fi
    if ! counts=$(cd "$workspace" && bead list --ready --json --limit 10000 2>/dev/null | eligible_counts); then
        echo "backlog SLO: ready query failed for $workspace" >&2
        exit 2
    fi
    jq -cn \
        --arg workspace "$workspace" \
        --argjson raw "$(jq -r '.raw_ready' <<<"$counts")" \
        --argjson eligible "$(jq -r '.eligible_ready' <<<"$counts")" \
        --argjson roam_accessible "$(grep -Fxq "$workspace" "$roam_paths" && echo true || echo false)" \
        '{workspace:$workspace, raw_ready:$raw, eligible_ready:$eligible,
          roam_accessible:$roam_accessible}' >>"$rows"
done <"$paths"

report=$(jq -sc \
    --slurpfile routes "$pinned_routes" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson workers "$FLEET_WORKER_TARGET" \
    --argjson roaming_workers "$roaming_workers" \
    --argjson target "$FLEET_ELIGIBLE_TARGET" \
    --argjson minimum "$FLEET_ELIGIBLE_MINIMUM" \
    --argjson target_per "$ELIGIBLE_TARGET_PER_WORKER" \
    --argjson minimum_per "$ELIGIBLE_MINIMUM_PER_WORKER" \
    --argjson reserve "$WORKSPACE_ELIGIBLE_RESERVE" '
      . as $workspace_rows
      | (reduce $routes[] as $route ({}; .[$route.workspace] = $route.workers)) as $pinned_map
      | (map(. + {pinned_workers: ($pinned_map[.workspace] // 0)})) as $pools
      | ($pools | map(select(.pinned_workers > 0)
          | . + {
              eligible_target: (.pinned_workers * $target_per),
              eligible_minimum: (.pinned_workers * $minimum_per)
            }
          | . + {status:
              (if .eligible_ready < .eligible_minimum then "breach"
               elif .eligible_ready < .eligible_target then "reserve"
               else "healthy" end)})) as $pinned_pools
      | ($pools | map(select(.roam_accessible)
          | ([.eligible_ready - (.pinned_workers * $minimum_per), 0] | max))
          | add // 0) as $roam_min_available
      | ($pools | map(select(.roam_accessible)
          | ([.eligible_ready - (.pinned_workers * $target_per), 0] | max))
          | add // 0) as $roam_target_available
      | ($roaming_workers * $minimum_per) as $roam_minimum
      | ($roaming_workers * $target_per) as $roam_target
      | ([$pinned_pools[] | select(.status == "breach")] | length) as $pinned_breaches
      | ([$pinned_pools[] | select(.status != "healthy")] | length) as $pinned_reserves
      | ($pools | map(.eligible_ready) | add // 0) as $eligible
      | ($pools | map(.raw_ready) | add // 0) as $raw
      | (if ($pinned_breaches > 0 or $roam_min_available < $roam_minimum) then "breach"
         elif ($pinned_reserves > 0 or $roam_target_available < $roam_target) then "reserve"
         else "healthy" end) as $status
      | {
          event: "needle.backlog.slo",
          timestamp: $timestamp,
          status: $status,
          worker_target: $workers,
          eligible_target: $target,
          eligible_minimum: $minimum,
          eligible_target_per_worker: $target_per,
          eligible_minimum_per_worker: $minimum_per,
          workspace_reserve: $reserve,
          workspaces: ($pools | length),
          raw_ready: $raw,
          eligible_ready: $eligible,
          pinned_pools: $pinned_pools,
          roaming_pool: {
            workers: $roaming_workers,
            eligible_minimum: $roam_minimum,
            eligible_target: $roam_target,
            eligible_after_pinned_minimum: $roam_min_available,
            eligible_after_pinned_target: $roam_target_available,
            status: (if $roam_min_available < $roam_minimum then "breach"
                     elif $roam_target_available < $roam_target then "reserve"
                     else "healthy" end)
          },
          underprovisioned_routes:
            ([$pinned_pools[] | select(.status != "healthy")
              | {workspace, workers: .pinned_workers, eligible_ready,
                 eligible_minimum, eligible_target, status}]
             + (if $roam_target_available < $roam_target then
                  [{workspace:"<roaming-pool>", workers:$roaming_workers,
                    eligible_ready:$roam_target_available,
                    eligible_minimum:$roam_minimum,
                    eligible_target:$roam_target,
                    status:(if $roam_min_available < $roam_minimum then "breach" else "reserve" end)}]
                else [] end))
        }
    ' "$rows")

if [[ "$OUTPUT" == json ]]; then
    jq -c . <<<"$report"
else
    jq -r '"status=\(.status) eligible=\(.eligible_ready) target=\(.eligible_target) minimum=\(.eligible_minimum) raw=\(.raw_ready) workspaces=\(.workspaces)",
           (.underprovisioned_routes[]
             | "\(.status | ascii_upcase) \(.workspace) workers=\(.workers) eligible=\(.eligible_ready) minimum=\(.eligible_minimum) target=\(.eligible_target)")' <<<"$report"
fi

[[ "$(jq -r '.status' <<<"$report")" != breach ]] || exit 1
