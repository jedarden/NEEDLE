#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLANNER="$REPO_ROOT/scripts/nextest-shard-plan.sh"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/needle-nextest-shard-plan-XXXXXX")"
trap 'rm -rf -- "$TMP_ROOT"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

run_plan() {
  local history=$1
  local output=$2
  "$PLANNER" \
    --inventory "$TMP_ROOT/inventory.json" \
    --history "$history" \
    --output-dir "$output" \
    --shards 5
}

printf '%s\n' '{
  "test-count": 10,
  "rust-suites": {
    "suite-a": {
      "package-name": "needle",
      "binary-id": "needle",
      "binary-name": "needle",
      "status": "listed",
      "testcases": {
        "a": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "b": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "c": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "d": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "e": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "ignored": {"kind":"test","ignored":true,"filter-match":{"status":"matches"}},
        "filtered": {"kind":"test","ignored":false,"filter-match":{"status":"mismatch"}}
      }
    },
    "suite-b": {
      "package-name": "needle",
      "binary-id": "needle::integration_spawn",
      "binary-name": "integration_spawn",
      "status": "listed",
      "testcases": {
        "f": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "g": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "h": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "i": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}},
        "j": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}}
      }
    },
    "skipped-suite": {
      "package-name": "needle",
      "binary-id": "needle::not-archived",
      "binary-name": "not-archived",
      "status": "skipped",
      "testcases": {
        "not_selected": {"kind":"test","ignored":false,"filter-match":{"status":"matches"}}
      }
    }
  }
}' > "$TMP_ROOT/inventory.json"

printf '%s\n' '{
  "schema": "needle-nextest-duration-history/v1",
  "run_count": 2,
  "samples": {
    "[\"needle\",\"a\"]": [8000000, 10000000],
    "[\"needle\",\"b\"]": [8000000],
    "[\"needle\",\"c\"]": [7000000],
    "[\"needle\",\"d\"]": [6000000],
    "[\"needle\",\"e\"]": [5000000],
    "[\"needle::integration_spawn\",\"f\"]": [4000000],
    "[\"needle::integration_spawn\",\"g\"]": [3000000],
    "[\"needle::integration_spawn\",\"h\"]": [2000000],
    "[\"needle::integration_spawn\",\"i\"]": [1000000],
    "[\"needle::integration_spawn\",\"j\"]": [1000000]
  }
}' > "$TMP_ROOT/history.json"

run_plan "$TMP_ROOT/history.json" "$TMP_ROOT/recorded-a" > "$TMP_ROOT/recorded-a.log"
run_plan "$TMP_ROOT/history.json" "$TMP_ROOT/recorded-b" > "$TMP_ROOT/recorded-b.log"

cmp "$TMP_ROOT/recorded-a/plan.json" "$TMP_ROOT/recorded-b/plan.json" \
  || fail 'identical inventory/history did not produce a byte-identical plan'
cmp "$TMP_ROOT/recorded-a/plan.sha256" "$TMP_ROOT/recorded-b/plan.sha256" \
  || fail 'identical inventory/history did not produce an identical plan digest'

jq -e '
  .schema == "needle-nextest-shard-plan/v1" and
  .shard_count == 5 and .inventory_count == 10 and
  .history_status == "recorded" and .consumed_samples == 11 and
  (.assignments | map(.binary_id + "$" + .testcase) | length) == 10
' "$TMP_ROOT/recorded-a/plan.json" >/dev/null \
  || fail 'recorded plan metadata is incomplete'

[[ "$(jq -r '.assignments[] | select(.binary_id == "needle" and .testcase == "a") | .estimated_duration_us' "$TMP_ROOT/recorded-a/plan.json")" == 9000000 ]] \
  || fail 'planner did not use the even-sample median'
[[ "$(jq -r '.assignments[] | select(.binary_id == "needle" and .testcase == "a") | .result_name' "$TMP_ROOT/recorded-a/plan.json")" == 'needle::needle$a' ]] \
  || fail 'planner did not derive nextest structured-result identity for the library binary'

expected_loads=$'1\t10000000\n2\t9000000\n3\t9000000\n4\t9000000\n5\t9000000'
actual_loads="$(jq -r '.assignments | group_by(.shard)[] | "\(.[0].shard)\t\(map(.estimated_duration_us) | add)"' "$TMP_ROOT/recorded-a/plan.json")"
[[ "$actual_loads" == "$expected_loads" ]] \
  || fail "unexpected deterministic LPT loads: $actual_loads"

identities="$(jq -r '.assignments[] | [.binary_id,.testcase] | @tsv' "$TMP_ROOT/recorded-a/plan.json" | LC_ALL=C sort)"
expected_identities=$'needle\ta\nneedle\tb\nneedle\tc\nneedle\td\nneedle\te\nneedle::integration_spawn\tf\nneedle::integration_spawn\tg\nneedle::integration_spawn\th\nneedle::integration_spawn\ti\nneedle::integration_spawn\tj'
[[ "$identities" == "$expected_identities" ]] \
  || fail 'plan union did not contain every matching non-ignored test exactly once'

for shard in 1 2 3 4 5; do
  [[ -s "$TMP_ROOT/recorded-a/shard-${shard}.filter" ]] \
    || fail "shard $shard filter is missing"
  [[ -s "$TMP_ROOT/recorded-a/shard-${shard}.filter.sha256" ]] \
    || fail "shard $shard filter digest is missing"
done
grep -Fq 'binary_id(=needle) & test(=a)' "$TMP_ROOT/recorded-a/shard-1.filter" \
  || fail 'filter does not use exact binary and testcase predicates'

run_plan "$TMP_ROOT/missing-history.json" "$TMP_ROOT/bootstrap" > "$TMP_ROOT/bootstrap.log"
jq -e '
  .history_status == "bootstrap" and .consumed_samples == 0 and
  ([.assignments[].estimated_duration_us] | unique) == [100000]
' "$TMP_ROOT/bootstrap/plan.json" >/dev/null \
  || fail 'missing history did not select the deterministic bootstrap duration'

printf '%s\n' '{"schema":"needle-nextest-duration-history/v1","run_count":1,"samples":{"[\"needle\",\"a\"]":[0]}}' \
  > "$TMP_ROOT/bad-history.json"
if run_plan "$TMP_ROOT/bad-history.json" "$TMP_ROOT/bad-history-output" >/dev/null 2>&1; then
  fail 'malformed duration history was accepted'
fi

jq '."test-count" = 11' "$TMP_ROOT/inventory.json" > "$TMP_ROOT/bad-count.json"
if "$PLANNER" --inventory "$TMP_ROOT/bad-count.json" --history "$TMP_ROOT/history.json" \
    --output-dir "$TMP_ROOT/bad-count-output" --shards 5 >/dev/null 2>&1; then
  fail 'inventory with a lying test-count was accepted'
fi

if "$PLANNER" --inventory "$TMP_ROOT/inventory.json" --history "$TMP_ROOT/history.json" \
    --output-dir "$TMP_ROOT/too-many-shards" --shards 11 >/dev/null 2>&1; then
  fail 'planner accepted an empty-shard topology'
fi

echo 'PASS: nextest shard planner is deterministic and exact-once'
echo 'PASS: recorded medians drive stable LPT balancing'
echo 'PASS: bootstrap, malformed history, and invalid inventory fail as specified'
