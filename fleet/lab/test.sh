#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SRC_DIR/workers.tsv"

bash -n "$SRC_DIR/apply-lab-fleet.sh"

rows=$(awk -F'\t' '$1 !~ /^#/ && NF == 5 {print}' "$MANIFEST")
[[ "$(wc -l <<<"$rows")" -eq 7 ]]
[[ "$(cut -f1 <<<"$rows" | sort -u | wc -l)" -eq 7 ]]
[[ "$(awk -F'\t' '$1 !~ /^lab-/ {n++} END {print n+0}' <<<"$rows")" -eq 0 ]]
[[ "$(awk -F'\t' '$3 != "claude-code-glm-5.3-flash" {n++} END {print n+0}' <<<"$rows")" -eq 0 ]]
[[ "$(awk -F'\t' '$5 != "false" {n++} END {print n+0}' <<<"$rows")" -eq 0 ]]
grep -q $'^lab-tgplat\t/home/coding/tradegraph-platform\tclaude-code-glm-5.3-flash\t30\tfalse$' "$MANIFEST"
! grep -q $'^lab-needle\t' "$MANIFEST"
grep -q 'default.target.wants"/needle-worker@\*.service' "$SRC_DIR/apply-lab-fleet.sh"
grep -q 'systemctl --user disable' "$SRC_DIR/apply-lab-fleet.sh"
grep -q 'systemctl --user mask' "$SRC_DIR/apply-lab-fleet.sh"

echo "lab fleet policy tests passed"
