#!/usr/bin/env bash
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SRC_DIR/workers.tsv"
ADAPTER_POLICY_CHECK="$SRC_DIR/../../scripts/check-adapter-cargo-environment.sh"

bash -n "$ADAPTER_POLICY_CHECK" "$SRC_DIR/apply-lab-fleet.sh"
"$ADAPTER_POLICY_CHECK" --self-test

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

# Both wrappers are tracked and installed by the same flag. Both must carry
# both hardenings: --slice keeps a needle.slice-contained worker's scope in
# needle.slice instead of app.slice (claudego-dd2fa6f4 for cargo-remote —
# the whole reason it was tracked — and claudego-4746d945 for bin/cargo's
# non-test fallback), and RuntimeMaxSec reaps a hung scope after 4h instead
# of never (needle-3d5c65d8).
for wrapper in cargo cargo-remote; do
    [[ -f "$SRC_DIR/bin/$wrapper" ]] || { echo "missing tracked wrapper: bin/$wrapper" >&2; exit 1; }
    bash -n "$SRC_DIR/bin/$wrapper"
done
for wrapper in cargo cargo-remote; do
    grep -q -- '--slice="$(current_slice)"' "$SRC_DIR/bin/$wrapper" \
        || { echo "bin/$wrapper lost --slice hardening" >&2; exit 1; }
    grep -q 'RuntimeMaxSec=14400' "$SRC_DIR/bin/$wrapper" \
        || { echo "bin/$wrapper lost RuntimeMaxSec" >&2; exit 1; }
done

# Drift check: parses, and its comparator detects every divergence shape.
bash -n "$SRC_DIR/bin/check-wrapper-drift.sh"
"$SRC_DIR/bin/check-wrapper-drift.sh" --self-test > /dev/null
grep -q 'wrapper-drift.timer' "$SRC_DIR/apply-lab-fleet.sh" \
    || { echo "apply-lab-fleet.sh does not enable the drift timer" >&2; exit 1; }
grep -q -- '--wrappers-only' "$SRC_DIR/apply-lab-fleet.sh" \
    || { echo "apply-lab-fleet.sh lost --wrappers-only" >&2; exit 1; }
# The wrappers-only exit must precede the manifest sanity loop, so codinghome
# never converges the lab manifest.
awk '/^if \[\[ "\$WRAPPERS_ONLY" == 1 \]\]; then/{w=NR} /^while IFS=\$.\t. read -r id ws agent delay explore; do/{if (w && NR < w) {print "wrappers-only exit is after the manifest sanity loop"; exit 1}}' "$SRC_DIR/apply-lab-fleet.sh"
for unit in wrapper-drift.service wrapper-drift.timer; do
    [[ -f "$SRC_DIR/$unit" ]] || { echo "missing drift unit: $unit" >&2; exit 1; }
done

echo "lab fleet policy tests passed"
