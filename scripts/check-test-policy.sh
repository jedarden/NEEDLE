#!/usr/bin/env bash
# Enforce the repository's explicit test tiers, growth budgets, and ownership.
# Keep this dependency-free: the CI builder deliberately contains only the
# tools needed by NEEDLE's Rust build, plus POSIX text utilities.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${NEEDLE_TEST_POLICY_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
POLICY_FILE="${NEEDLE_TEST_POLICY_FILE:-tests/test-policy.toml}"
[[ "$POLICY_FILE" == /* ]] || POLICY_FILE="$REPO_ROOT/$POLICY_FILE"
MANIFEST="$REPO_ROOT/Cargo.toml"

declare -a ERRORS=()
declare -a ARTIFACT_PATTERNS=()
declare -a ARTIFACT_OWNERS=()
declare -A CARGO_PATH=()
declare -A POLICY_PATH=()
declare -A POLICY_TIER=()
declare -A POLICY_MAX=()
declare -A POLICY_OWNER=()
declare -A TIER_BUDGET=()
declare -A TIER_COUNT=()
declare -A HARNESS_OWNED=()
declare -A EXCEPTION_MAX=()
declare -A EXCEPTION_OWNER=()
declare -A FINDING_COUNT=()

error() {
  ERRORS+=("$1")
}

is_nonnegative_integer() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

artifact_matches() {
  local path="$1" pattern="$2"
  if [[ "$pattern" == */\* ]]; then
    # A directory rule owns its complete subtree.
    [[ "$path" == "${pattern%\*}"* ]]
  else
    # Filename globs do not cross a directory boundary.
    [[ "${path%/*}" == "${pattern%/*}" && "${path##*/}" == ${pattern##*/} ]]
  fi
}

# Parse only the deliberately small policy schema. Rejecting a malformed or
# unknown structure here is safer than silently interpreting general TOML.
policy_records() {
  awk '
    function trim(value) {
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      return value
    }
    function clear_values(key) {
      for (key in value) delete value[key]
    }
    function emit() {
      if (section == "harness") {
        printf "harness\t%s\t%s\t%s\t%s\t%s\n", value["name"], value["path"], value["tier"], value["max_tests"], value["owner"]
      } else if (section == "unit_exception") {
        printf "exception\t%s\t%s\t%s\t%s\t%s\n", value["path"], value["kind"], value["max_occurrences"], value["owner"], value["reason"]
      } else if (section == "artifact") {
        printf "artifact\t%s\t%s\n", value["pattern"], value["owner"]
      }
      clear_values()
    }
    /^[[:space:]]*(#|$)/ { next }
    /^[[:space:]]*\[\[(harness|unit_exception|artifact)\]\][[:space:]]*$/ {
      emit()
      line = $0
      gsub(/^[[:space:]]*\[\[|\]\][[:space:]]*$/, "", line)
      section = line
      next
    }
    /^[[:space:]]*\[(unit|tier_budget)\][[:space:]]*$/ {
      emit()
      line = $0
      gsub(/^[[:space:]]*\[|\][[:space:]]*$/, "", line)
      section = line
      next
    }
    /^[[:space:]]*[A-Za-z_][A-Za-z0-9_-]*[[:space:]]*=/ {
      line = $0
      key = line
      sub(/[[:space:]]*=.*/, "", key)
      key = trim(key)
      sub(/^[^=]*=[[:space:]]*/, "", line)
      line = trim(line)
      if (line ~ /^".*"$/) {
        sub(/^"/, "", line)
        sub(/"$/, "", line)
      }
      if (section == "") {
        printf "root\t%s\t%s\n", key, line
      } else if (section == "unit") {
        printf "unit\t%s\t%s\n", key, line
      } else if (section == "tier_budget") {
        printf "tier\t%s\t%s\n", key, line
      } else {
        value[key] = line
      }
      next
    }
    { printf "invalid\t%d\t%s\n", NR, $0 }
    END { emit() }
  ' "$POLICY_FILE"
}

if [[ ! -f "$POLICY_FILE" ]]; then
  echo "FAIL: cannot read test policy: $POLICY_FILE" >&2
  exit 1
fi
if [[ ! -f "$MANIFEST" ]]; then
  echo "FAIL: cannot read Cargo manifest: $MANIFEST" >&2
  exit 1
fi

POLICY_VERSION=""
UNIT_MAX=""
while IFS=$'\t' read -r record first second third fourth fifth; do
  case "$record" in
    root)
      if [[ "$first" == version ]]; then POLICY_VERSION="$second"; else error "unknown root policy key: $first"; fi
      ;;
    unit)
      if [[ "$first" == max_tests ]]; then UNIT_MAX="$second"; else error "unknown unit policy key: $first"; fi
      ;;
    tier)
      TIER_BUDGET["$first"]="$second"
      ;;
    harness)
      name="$first" path="$second" tier="$third" maximum="$fourth" owner="$fifth"
      if [[ -z "$name" ]]; then error "every [[harness]] entry must have a non-empty name"; continue; fi
      if [[ -n "${POLICY_PATH[$name]+x}" ]]; then error "duplicate harness policy: $name"; continue; fi
      POLICY_PATH["$name"]="$path"
      POLICY_TIER["$name"]="$tier"
      POLICY_MAX["$name"]="$maximum"
      POLICY_OWNER["$name"]="$owner"
      ;;
    exception)
      path="$first" kind="$second" maximum="$third" owner="$fourth" reason="$fifth"
      key="$path|$kind"
      if [[ -n "${EXCEPTION_MAX[$key]+x}" ]]; then error "duplicate unit exception: $path $kind"; continue; fi
      if [[ "$path" != src/* ]]; then error "unit exception has invalid path: $path"; fi
      case "$kind" in sleep|process|global-env) ;; *) error "unit exception $path has invalid kind: $kind" ;; esac
      if ! is_nonnegative_integer "$maximum" || [[ "$maximum" == 0 ]]; then
        error "unit exception $path $kind has invalid max_occurrences"
      fi
      if [[ -z "$owner" ]]; then error "unit exception $path $kind has no owner"; fi
      if [[ -z "$reason" ]]; then error "unit exception $path $kind has no reason"; fi
      EXCEPTION_MAX["$key"]="$maximum"
      EXCEPTION_OWNER["$key"]="$owner"
      ;;
    artifact)
      ARTIFACT_PATTERNS+=("$first")
      ARTIFACT_OWNERS+=("$second")
      if [[ "$first" != tests/* ]]; then error "artifact rule has invalid pattern: $first"; fi
      if [[ -z "$second" ]]; then error "artifact rule $first has no owner"; fi
      ;;
    invalid)
      error "invalid test policy syntax at line $first: $second"
      ;;
  esac
done < <(policy_records)

[[ "$POLICY_VERSION" == 1 ]] || error "policy version must be 1"
if ! is_nonnegative_integer "$UNIT_MAX"; then error "unit tier has invalid max_tests: $UNIT_MAX"; fi

ALLOWED_TIERS=(push contract e2e soak fuzz)
for tier in "${ALLOWED_TIERS[@]}"; do
  if [[ -z "${TIER_BUDGET[$tier]+x}" ]]; then
    error "tier budgets missing: $tier"
  elif ! is_nonnegative_integer "${TIER_BUDGET[$tier]}"; then
    error "tier $tier has invalid budget: ${TIER_BUDGET[$tier]}"
  fi
  TIER_COUNT["$tier"]=0
done
for tier in "${!TIER_BUDGET[@]}"; do
  case "$tier" in push|contract|e2e|soak|fuzz) ;; *) error "unknown tier budget: $tier" ;; esac
done

if ! grep -Eq '^autotests[[:space:]]*=[[:space:]]*false([[:space:]]*(#.*)?)?$' "$MANIFEST"; then
  error "Cargo.toml must set package.autotests = false"
fi

while IFS=$'\t' read -r name path; do
  if [[ -n "$name" && -n "$path" ]]; then
    if [[ -n "${CARGO_PATH[$name]+x}" ]]; then error "duplicate Cargo test target: $name"; fi
    CARGO_PATH["$name"]="$path"
  else
    error "every [[test]] target must have string name and path fields"
  fi
done < <(awk '
  function emit() { if (in_test) printf "%s\t%s\n", name, path }
  /^\[\[test\]\][[:space:]]*$/ { emit(); in_test=1; name=""; path=""; next }
  /^\[/ { emit(); in_test=0 }
  in_test && /^[[:space:]]*name[[:space:]]*=/ {
    line=$0; sub(/^[^=]*=[[:space:]]*"/, "", line); sub(/".*$/, "", line); name=line
  }
  in_test && /^[[:space:]]*path[[:space:]]*=/ {
    line=$0; sub(/^[^=]*=[[:space:]]*"/, "", line); sub(/".*$/, "", line); path=line
  }
  END { emit() }
' "$MANIFEST")

count_test_attributes() {
  awk '/^[[:space:]]*#[[:space:]]*\[[[:space:]]*(tokio::)?test([[:space:]]*\([^]]*\))?[[:space:]]*\]/{count++} END{print count+0}' "$@"
}

for name in "${!POLICY_PATH[@]}"; do
  path="${POLICY_PATH[$name]}" tier="${POLICY_TIER[$name]}" maximum="${POLICY_MAX[$name]}" owner="${POLICY_OWNER[$name]}"
  case "$tier" in push|contract|e2e|soak|fuzz) ;; *) error "harness $name has invalid or missing tier: $tier" ;; esac
  if [[ -z "$owner" ]]; then error "harness $name has no owner"; fi
  if ! is_nonnegative_integer "$maximum"; then error "harness $name has invalid max_tests: $maximum"; continue; fi
  if [[ -z "${CARGO_PATH[$name]+x}" ]]; then
    error "policy declares harness absent from Cargo.toml: $name"
  elif [[ "${CARGO_PATH[$name]}" != "$path" ]]; then
    error "harness $name path differs: policy=$path, Cargo.toml=${CARGO_PATH[$name]}"
  fi
  if [[ ! -f "$REPO_ROOT/$path" ]]; then error "harness $name source does not exist: $path"; continue; fi

  files=("$REPO_ROOT/$path")
  module_dir="$REPO_ROOT/${path%.rs}"
  if [[ -d "$module_dir" ]]; then
    while IFS= read -r -d '' file; do files+=("$file"); done < <(find "$module_dir" -type f -name '*.rs' -print0 | sort -z)
  fi
  actual="$(count_test_attributes "${files[@]}")"
  if (( actual > maximum )); then error "harness $name exceeds growth budget: $actual > $maximum"; fi
  case "$tier" in push|contract|e2e|soak|fuzz) TIER_COUNT["$tier"]=$((TIER_COUNT[$tier] + actual)) ;; esac
  for file in "${files[@]}"; do HARNESS_OWNED["${file#"$REPO_ROOT/"}"]=1; done
done

for name in "${!CARGO_PATH[@]}"; do
  if [[ -z "${POLICY_PATH[$name]+x}" ]]; then error "Cargo integration harness has no test tier: $name"; fi
done

while IFS= read -r -d '' file; do
  path="${file#"$REPO_ROOT/"}"
  classified=false
  for name in "${!CARGO_PATH[@]}"; do
    if [[ "${CARGO_PATH[$name]}" == "$path" ]]; then classified=true; break; fi
  done
  if [[ "$classified" == false ]]; then error "unclassified top-level Rust test: $path"; fi
done < <(find "$REPO_ROOT/tests" -maxdepth 1 -type f -name '*.rs' -print0 | sort -z)

for tier in "${ALLOWED_TIERS[@]}"; do
  if is_nonnegative_integer "${TIER_BUDGET[$tier]:-}" && (( TIER_COUNT[$tier] > TIER_BUDGET[$tier] )); then
    error "tier $tier exceeds growth budget: ${TIER_COUNT[$tier]} > ${TIER_BUDGET[$tier]}"
  fi
done

mapfile -d '' UNIT_FILES < <(find "$REPO_ROOT/src" -type f -name '*.rs' -print0 | sort -z)
UNIT_TESTS="$(count_test_attributes "${UNIT_FILES[@]}")"
if is_nonnegative_integer "$UNIT_MAX" && (( UNIT_TESTS > UNIT_MAX )); then
  error "unit tier exceeds growth budget: $UNIT_TESTS > $UNIT_MAX"
fi

while IFS=$'\t' read -r path kind count lines; do
  key="$path|$kind"
  FINDING_COUNT["$key"]="$count"
  if [[ -z "${EXCEPTION_MAX[$key]+x}" ]]; then
    error "unit tier uses raw $kind without an owned exception: $path:$lines"
  elif is_nonnegative_integer "${EXCEPTION_MAX[$key]}" && (( count > EXCEPTION_MAX[$key] )); then
    error "unit exception budget exceeded for $path $kind: $count > ${EXCEPTION_MAX[$key]}"
  fi
done < <(cd "$REPO_ROOT" && awk -f "$SCRIPT_DIR/check-test-policy.awk" "${UNIT_FILES[@]#"$REPO_ROOT/"}")

for key in "${!EXCEPTION_MAX[@]}"; do
  if [[ -z "${FINDING_COUNT[$key]+x}" ]]; then error "stale unit exception has no matching hazard: ${key/|/ }"; fi
done

POLICY_REL="${POLICY_FILE#"$REPO_ROOT/"}"
while IFS= read -r -d '' file; do
  path="${file#"$REPO_ROOT/"}"
  [[ "$path" == "$POLICY_REL" || -n "${HARNESS_OWNED[$path]+x}" ]] && continue
  owned=false
  for index in "${!ARTIFACT_PATTERNS[@]}"; do
    pattern="${ARTIFACT_PATTERNS[$index]}"
    if [[ -n "${ARTIFACT_OWNERS[$index]}" ]] && artifact_matches "$path" "$pattern"; then owned=true; break; fi
  done
  if [[ "$owned" == false ]]; then error "verification-only test artifact has no owner: $path"; fi
done < <(find "$REPO_ROOT/tests" -type f -print0 | sort -z)

if [[ ${#ERRORS[@]} -gt 0 ]]; then
  echo "FAIL: test policy has ${#ERRORS[@]} violation(s)" >&2
  for message in "${ERRORS[@]}"; do echo "  - $message" >&2; done
  exit 1
fi

echo "PASS: all test harnesses have explicit tiers and stay within growth budgets"
echo "PASS: unit-tier hazards and verification artifacts have explicit owners"
