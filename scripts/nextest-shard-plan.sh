#!/usr/bin/env bash
# Build deterministic, duration-balanced nextest filtersets from a list JSON.
set -euo pipefail

usage() {
  echo "usage: $0 --inventory FILE --history FILE --output-dir DIR --shards N" >&2
  exit 2
}

fail() {
  echo "ERROR: nextest shard planner: $*" >&2
  exit 1
}

inventory=
history=
output_dir=
shard_count=
while (($#)); do
  case "$1" in
    --inventory) (($# >= 2)) || usage; inventory=$2; shift 2 ;;
    --history) (($# >= 2)) || usage; history=$2; shift 2 ;;
    --output-dir) (($# >= 2)) || usage; output_dir=$2; shift 2 ;;
    --shards) (($# >= 2)) || usage; shard_count=$2; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$inventory" && -n "$history" && -n "$output_dir" && -n "$shard_count" ]] || usage
[[ "$shard_count" =~ ^[1-9][0-9]*$ ]] || fail "shard count must be a positive integer"
[[ -s "$inventory" ]] || fail "inventory is missing or empty: $inventory"
command -v jq >/dev/null 2>&1 || fail "jq is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
mkdir -p "$output_dir"

work_dir="$output_dir/.work"
mkdir -p "$work_dir"
inventory_tsv="$work_dir/inventory.tsv"
candidates_tsv="$work_dir/candidates.tsv"
sorted_tsv="$work_dir/sorted.tsv"
assignments_tsv="$work_dir/assignments.tsv"
history_source="$history"
history_status=recorded

if [[ ! -e "$history" ]]; then
  history_status=bootstrap
  history_source="$work_dir/bootstrap-history.json"
  printf '%s\n' '{"schema":"needle-nextest-duration-history/v1","run_count":0,"samples":{}}' \
    > "$history_source"
fi

jq -e '
  type == "object" and
  .schema == "needle-nextest-duration-history/v1" and
  (.run_count | type == "number" and floor == . and . >= 0) and
  (.samples | type == "object") and
  all(.samples[];
    type == "array" and length >= 1 and length <= 9 and
    all(.[]; type == "number" and floor == . and . >= 1 and . <= 3600000000))
' "$history_source" >/dev/null || fail "duration history is malformed"

jq -e '
  type == "object" and
  (."test-count" | type == "number" and floor == . and . >= 0) and
  (."rust-suites" | type == "object")
' "$inventory" >/dev/null || fail "nextest inventory has an unknown schema"

jq -r '
  ."rust-suites" | to_entries[] |
  .value as $suite |
  select($suite.status == "listed") |
  $suite.testcases | to_entries[] |
  select(.value.kind == "test") |
  select(.value.ignored == false) |
  select(.value."filter-match".status == "matches") |
  [$suite."binary-id", .key, ($suite."package-name" + "::" + $suite."binary-name")] | @tsv
' "$inventory" | LC_ALL=C sort -t $'\t' -k1,1 -k2,2 > "$inventory_tsv"

inventory_count=$(wc -l < "$inventory_tsv" | tr -d '[:space:]')
reported_count=$(jq -r '."test-count"' "$inventory")
listed_count=$(jq '[."rust-suites" | to_entries[] |
  select(.value.status == "listed") | .value.testcases | length] | add // 0' "$inventory")
[[ "$listed_count" =~ ^[0-9]+$ && "$listed_count" == "$reported_count" ]] ||
  fail "listed testcase count $listed_count differs from nextest test-count $reported_count"
((inventory_count >= shard_count)) ||
  fail "inventory has $inventory_count tests but $shard_count nonempty shards are required"

if [[ -n "$(cut -f1-2 "$inventory_tsv" | LC_ALL=C sort | uniq -d)" ]]; then
  fail "nextest inventory contains a duplicate binary/test identity"
fi
if [[ -n "$(awk -F '\t' '{ print $3 "$" $2 }' "$inventory_tsv" | LC_ALL=C sort | uniq -d)" ]]; then
  fail "nextest inventory contains a duplicate structured-result identity"
fi
binary_id_pattern='^[A-Za-z0-9_.:-]+$'
testcase_pattern='^[A-Za-z0-9_.: -]+$'
while IFS=$'\t' read -r binary_id testcase result_prefix extra; do
  [[ -n "$result_prefix" && -z "${extra:-}" ]] || fail "inventory row has an invalid shape"
  [[ "$binary_id" =~ $binary_id_pattern ]] ||
    fail "binary id contains a character unsupported by the exact filter encoder"
  [[ "$testcase" =~ $testcase_pattern ]] ||
    fail "test name contains a character unsupported by the exact filter encoder"
  [[ "$result_prefix" =~ $binary_id_pattern ]] ||
    fail "structured-result prefix contains an unsupported character"
  [[ "$binary_id" != *'$'* && "$testcase" != *'$'* && "$result_prefix" != *'$'* ]] ||
    fail "inventory identity contains the structured-result separator"
done < "$inventory_tsv"

# Emit estimated durations in integer microseconds. The median is stable for
# both odd and even sample counts; new tests receive a conservative 100 ms.
jq -Rnr --slurpfile history "$history_source" '
  def median:
    sort as $s |
    ($s | length) as $n |
    if $n == 0 then 100000
    elif ($n % 2) == 1 then $s[($n / 2 | floor)]
    else (($s[$n / 2 - 1] + $s[$n / 2]) / 2 | floor)
    end;
  inputs |
  split("\t") as $fields |
  ([$fields[0], $fields[1]] | @json) as $identity |
  [($history[0].samples[$identity] // [] | median), $fields[0], $fields[1], $fields[2]] |
  @tsv
' < "$inventory_tsv" > "$candidates_tsv"

LC_ALL=C sort -t $'\t' -k1,1nr -k2,2 -k3,3 "$candidates_tsv" > "$sorted_tsv"

# Longest-processing-time first: assign each next test to the least-loaded
# shard, breaking equal-load ties by the lowest shard number.
awk -F '\t' -v shards="$shard_count" '
  BEGIN { OFS = "\t"; for (i = 1; i <= shards; i++) load[i] = 0 }
  {
    best = 1
    for (i = 2; i <= shards; i++) {
      if (load[i] < load[best]) best = i
    }
    load[best] += $1
    print best, $1, $2, $3, $4
  }
' "$sorted_tsv" > "$assignments_tsv"

[[ "$(wc -l < "$assignments_tsv" | tr -d '[:space:]')" == "$inventory_count" ]] ||
  fail "planner did not assign every inventory entry"
duplicate_assignment=$(cut -f3-4 "$assignments_tsv" | LC_ALL=C sort | uniq -d)
if [[ -n "$duplicate_assignment" ]]; then
  fail "planner assigned an identity more than once: $duplicate_assignment"
fi
if ! diff -u \
    <(cut -f1-2 "$inventory_tsv") \
    <(cut -f3-4 "$assignments_tsv" | LC_ALL=C sort -t $'\t' -k1,1 -k2,2) >/dev/null; then
  fail "planner assignment union differs from the inventory"
fi

for ((shard = 1; shard <= shard_count; shard++)); do
  shard_tsv="$output_dir/shard-${shard}.tsv"
  filter_path="$output_dir/shard-${shard}.filter"
  awk -F '\t' -v shard="$shard" '$1 == shard { print $2 "\t" $3 "\t" $4 "\t" $5 }' \
    "$assignments_tsv" |
    LC_ALL=C sort -t $'\t' -k2,2 -k3,3 > "$shard_tsv"
  [[ -s "$shard_tsv" ]] || fail "shard $shard is empty"

  : > "$filter_path"
  separator=
  while IFS=$'\t' read -r duration_us binary_id testcase result_prefix; do
    printf '%s(binary_id(=%s) & test(=%s))' "$separator" "$binary_id" "$testcase" \
      >> "$filter_path"
    separator=' | '
  done < "$shard_tsv"
  printf '\n' >> "$filter_path"
  [[ -s "$filter_path" ]] || fail "shard $shard filter is empty"
  sha256sum "$filter_path" | awk '{print $1}' > "$output_dir/shard-${shard}.filter.sha256"
done

inventory_sha256=$(sha256sum "$inventory_tsv" | awk '{print $1}')
history_sha256=$(sha256sum "$history_source" | awk '{print $1}')
consumed_samples=$(jq --slurpfile history "$history_source" -Rn '
  reduce inputs as $line (0;
    ($line | split("\t") | .[0:2] | @json) as $identity |
    . + ($history[0].samples[$identity] // [] | length))
' < "$inventory_tsv")

jq -Rn \
  --arg schema needle-nextest-shard-plan/v1 \
  --arg history_status "$history_status" \
  --arg history_sha256 "$history_sha256" \
  --arg inventory_sha256 "$inventory_sha256" \
  --argjson shard_count "$shard_count" \
  --argjson inventory_count "$inventory_count" \
  --argjson consumed_samples "$consumed_samples" '
  [inputs | split("\t") |
    {shard:(.[0] | tonumber), estimated_duration_us:(.[1] | tonumber),
     binary_id:.[2], testcase:.[3], result_name:(.[4] + "$" + .[3])}]
  | {
      schema:$schema,
      shard_count:$shard_count,
      inventory_count:$inventory_count,
      inventory_sha256:$inventory_sha256,
      history_status:$history_status,
      history_sha256:$history_sha256,
      consumed_samples:$consumed_samples,
      assignments:.
    }
' < "$assignments_tsv" > "$output_dir/plan.json"

jq -e --argjson shards "$shard_count" --argjson count "$inventory_count" '
  .schema == "needle-nextest-shard-plan/v1" and
  .shard_count == $shards and
  .inventory_count == $count and
  (.assignments | length) == $count
' "$output_dir/plan.json" >/dev/null || fail "canonical plan validation failed"
sha256sum "$output_dir/plan.json" | awk '{print $1}' > "$output_dir/plan.sha256"

printf 'NEEDLE_NEXTEST_SHARD_PLAN status=ok history=%s tests=%s shards=%s consumed_samples=%s plan_sha256=%s\n' \
  "$history_status" "$inventory_count" "$shard_count" "$consumed_samples" \
  "$(cat "$output_dir/plan.sha256")"
