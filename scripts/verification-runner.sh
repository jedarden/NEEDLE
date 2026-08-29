#!/usr/bin/env bash
# Configurable definition-of-done runner.
#
# The configuration is YAML so repositories can keep their verification policy
# separate from this reusable execution engine. Checks are run sequentially,
# but a failure never prevents a later check from running.
#
# Usage:
#   scripts/verification-runner.sh [--fast|--slow|--all]
#       [--config PATH] [--json PATH] [--verbose] [--dry-run]
#       [--count-bypass] [--no-verify]
#
# Exit codes:
#   0 - all selected checks passed (allowed failures do not count as failures)
#   1 - one or more selected checks failed
#   2 - configuration, dependency, or report-output error
#   3 - invalid command-line arguments

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  REPO_ROOT="$(cd "$REPO_ROOT" && pwd)"
else
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi
cd "$REPO_ROOT"

LANE="all"
CONFIG_PATH=""
VERBOSE=false
DRY_RUN=false
COUNT_BYPASS=false
EXPLICIT_BYPASS=false
JSON_PATH=""
CONFIG_JSON=""

declare -a RESULT_LANES=()
declare -a RESULT_NAMES=()
declare -a RESULT_STATUS=()
declare -a RESULT_EXIT_CODES=()
declare -a RESULT_STDOUT=()
declare -a RESULT_STDERR=()
declare -a CURRENT_ARGS=()
declare -a CURRENT_ENV=()

TOTAL_CHECKS=0
TOTAL_PASSED=0
TOTAL_ALLOWED_FAILURES=0
TOTAL_SKIPPED=0
TOTAL_FAILED=0

if [[ -t 1 && "${NO_COLOR:-}" != 1 ]]; then
  readonly RED=$'\033[0;31m'
  readonly GREEN=$'\033[0;32m'
  readonly YELLOW=$'\033[0;33m'
  readonly BLUE=$'\033[0;34m'
  readonly RESET=$'\033[0m'
else
  readonly RED=''
  readonly GREEN=''
  readonly YELLOW=''
  readonly BLUE=''
  readonly RESET=''
fi

usage() {
  cat <<'EOF'
Usage: scripts/verification-runner.sh [OPTIONS]

Execute definition-of-done checks from a YAML configuration. Every check in
the selected lane runs, even when an earlier check fails.

OPTIONS:
  --fast              Run fast_lane only
  --slow              Run slow_lane only
  --all               Run fast_lane and slow_lane (default)
  --config PATH       Read this configuration instead of auto-detecting one
  --json PATH         Write a machine-readable result report to PATH
  --verbose           Print captured stdout and stderr for every check
  --dry-run           Validate and list checks without executing them
  --count-bypass      Record pre-commit verification state when helpers exist
  --no-verify         Explicitly skip verification and record a bypass
  --help, -h          Show this help

Configuration discovery (in order):
  .verification/config.yaml
  definition-of-done.yaml
  .verification/config.yml
  definition-of-done.yml

The configuration format is documented in docs/verification-runner.md.
EOF
}

log_info() {
  printf '%s[%s]%s %s\n' "$BLUE" "$(date -u +%H:%M:%S)" "$RESET" "$*"
}

log_success() {
  printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*"
}

log_error() {
  printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2
}

log_warn() {
  printf '%s⚠%s %s\n' "$YELLOW" "$RESET" "$*" >&2
}

is_truthy() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

lane_csv() {
  case "$1" in
    fast) printf '%s' fast ;;
    slow) printf '%s' slow ;;
    all) printf '%s' 'fast,slow' ;;
    *) printf '%s' "$1" ;;
  esac
}

parse_arguments() {
  while (($# > 0)); do
    case "$1" in
      --fast) LANE=fast; shift ;;
      --slow) LANE=slow; shift ;;
      --all) LANE=all; shift ;;
      --config)
        if [[ $# -lt 2 || -z "$2" ]]; then
          log_error '--config requires a path'
          exit 3
        fi
        CONFIG_PATH="$2"
        shift 2
        ;;
      --config=*) CONFIG_PATH="${1#*=}"; shift ;;
      --json)
        if [[ $# -lt 2 || -z "$2" ]]; then
          log_error '--json requires a path'
          exit 3
        fi
        JSON_PATH="$2"
        shift 2
        ;;
      --json=*) JSON_PATH="${1#*=}"; shift ;;
      --verbose) VERBOSE=true; shift ;;
      --dry-run) DRY_RUN=true; shift ;;
      --count-bypass) COUNT_BYPASS=true; shift ;;
      --no-verify) EXPLICIT_BYPASS=true; shift ;;
      --help|-h) usage; exit 0 ;;
      *)
        log_error "Unknown argument: $1"
        usage >&2
        exit 3
        ;;
    esac
  done
}

detect_config_path() {
  local candidate
  for candidate in \
    .verification/config.yaml \
    definition-of-done.yaml \
    .verification/config.yml \
    definition-of-done.yml; do
    if [[ -f "$REPO_ROOT/$candidate" ]]; then
      printf '%s/%s\n' "$REPO_ROOT" "$candidate"
      return 0
    fi
  done
  return 1
}

validate_config() {
  # A malformed check must fail the run rather than silently becoming a
  # passing run with zero checks.
  jq -e '
    def valid_lane:
      if . == null then true
      elif type != "array" then false
      else true end;
    def valid_check:
      type == "object"
      and ((.name | type) == "string" and (.name | length) > 0)
      and ((.command | type) == "string" and (.command | length) > 0)
      and (((.args // []) | if type == "array" then all(.[]; type == "string") else false end))
      and (((.timeout // 60) | if type == "number" then (. > 0 and floor == .) else false end))
      and (((.allow_failure // false) | type) == "boolean")
      and (((.environment // []) | if type == "array" then all(.[]; type == "string" and test("^[A-Za-z_][A-Za-z0-9_]*=")) else false end));
    type == "object"
    and ((.version | type) == "string" and (.version | length) > 0)
    and ((.fast_lane | valid_lane) and (.slow_lane | valid_lane))
    and (((.fast_lane // []) | all(.[]; valid_check)))
    and (((.slow_lane // []) | all(.[]; valid_check)))
  ' <<< "$CONFIG_JSON" >/dev/null
}

load_config() {
  local config_file="$1"

  if [[ ! -f "$config_file" ]]; then
    log_error "Configuration file not found: $config_file"
    return 1
  fi
  if ! command -v yq >/dev/null 2>&1; then
    log_error 'yq is required for YAML parsing but was not found in PATH'
    return 1
  fi
  if ! command -v jq >/dev/null 2>&1; then
    log_error 'jq is required for configuration validation and reports but was not found in PATH'
    return 1
  fi
  if ! command -v timeout >/dev/null 2>&1; then
    log_error 'GNU timeout is required to bound check execution but was not found in PATH'
    return 1
  fi

  if ! CONFIG_JSON="$(yq eval -o=json '.' "$config_file")"; then
    log_error "Unable to parse YAML configuration: $config_file"
    return 1
  fi
  if ! validate_config; then
    log_error "Invalid verification configuration: $config_file"
    log_error 'version must be a non-empty string; lanes must be lists; every check needs a name and command'
    log_error 'args must be strings, timeout must be a positive integer, and environment must contain KEY=value strings'
    return 1
  fi
  log_info "Loaded configuration from: $config_file"
}

fallback_bypass_requested() {
  is_truthy "${SKIP_CHECKS:-}" ||
    is_truthy "${VERIFICATION_SKIP:-}" ||
    is_truthy "${NEEDLE_SKIP_VERIFICATION:-}"
}

detect_bypass_pattern() {
  if [[ "$EXPLICIT_BYPASS" == true ]]; then
    printf '%s' '--no-verify'
  elif type needle_bypass_requested >/dev/null 2>&1 && needle_bypass_requested; then
    if type needle_bypass_pattern >/dev/null 2>&1; then
      needle_bypass_pattern
    else
      printf 'SKIP_CHECKS=%s' "${SKIP_CHECKS}"
    fi
  elif fallback_bypass_requested; then
    if is_truthy "${SKIP_CHECKS:-}"; then
      printf 'SKIP_CHECKS=%s' "${SKIP_CHECKS}"
    elif is_truthy "${VERIFICATION_SKIP:-}"; then
      printf 'VERIFICATION_SKIP=%s' "${VERIFICATION_SKIP}"
    else
      printf 'NEEDLE_SKIP_VERIFICATION=%s' "${NEEDLE_SKIP_VERIFICATION}"
    fi
  fi
}

append_bypass_event_fallback() {
  local record="$1"
  local log_file="${VERIFICATION_BYPASS_LOG:-$REPO_ROOT/.beads/bypasses.jsonl}"
  local lock_file="${log_file}.lock"
  local lock_fd
  local lock_dir
  local attempt
  local status=0

  mkdir -p "$(dirname "$log_file")" || return 1
  if command -v flock >/dev/null 2>&1; then
    if ! exec {lock_fd}>>"$lock_file"; then
      return 1
    fi
    if ! flock -x "$lock_fd"; then
      eval "exec ${lock_fd}>&-"
      return 1
    fi
    printf '%s\n' "$record" >> "$log_file" || status=1
    flock -u "$lock_fd" || status=1
    eval "exec ${lock_fd}>&-"
    return "$status"
  fi

  lock_dir="${lock_file}.d"
  for ((attempt = 1; attempt <= 3000; attempt++)); do
    if mkdir "$lock_dir" 2>/dev/null; then
      printf '%s\n' "$record" >> "$log_file" || status=1
      rmdir "$lock_dir" 2>/dev/null || status=1
      return "$status"
    fi
    sleep 0.01
  done
  return 1
}

record_bypass() {
  local pattern="$1"
  local lanes
  local timestamp
  local commit_sha
  local working_directory
  local hostname_value
  local username_value
  local record

  lanes="$(lane_csv "$LANE")"
  timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if command -v git >/dev/null 2>&1; then
    commit_sha="$(git rev-parse --verify HEAD 2>/dev/null || printf '%s' unknown)"
  else
    commit_sha=unknown
  fi
  working_directory="$(pwd -P)"
  hostname_value="$(hostname 2>/dev/null || uname -n 2>/dev/null || printf '%s' unknown)"
  username_value="$(id -un 2>/dev/null || printf '%s' "${USER:-unknown}")"

  if type needle_json_event >/dev/null 2>&1 && type needle_append_bypass_event >/dev/null 2>&1; then
    record="$(needle_json_event "$timestamp" "$commit_sha" "$lanes" "$pattern" 'Verification was explicitly skipped' "$working_directory")"
    if [[ -n "${VERIFICATION_BYPASS_LOG:-}" ]]; then
      NEEDLE_BYPASS_LOG="$VERIFICATION_BYPASS_LOG" needle_append_bypass_event "$record"
    else
      needle_append_bypass_event "$record"
    fi
    return $?
  fi

  record="$(jq -cn \
    --arg timestamp "$timestamp" \
    --arg commit_sha "$commit_sha" \
    --arg hostname "$hostname_value" \
    --arg username "$username_value" \
    --arg lanes "$lanes" \
    --arg pattern "$pattern" \
    --arg working_directory "$working_directory" \
    '{timestamp: $timestamp, commit_sha: $commit_sha, hostname: $hostname, username: $username, lanes_skipped: ($lanes | split(",") | map(select(length > 0))), pattern: $pattern, reason: "Verification was explicitly skipped", working_directory: $working_directory}')"
  append_bypass_event_fallback "$record"
}

handle_bypass() {
  local pattern="$1"
  local lanes

  lanes="$(lane_csv "$LANE")"
  if type needle_warn_bypass >/dev/null 2>&1; then
    needle_warn_bypass "$pattern" "$lanes"
  else
    log_warn "Definition of Done bypass detected: $pattern"
    log_warn "Verification lanes skipped: ${lanes//,/, }"
    log_warn 'This bypass will be recorded in the configured bypass log.'
  fi

  # With the existing NEEDLE hook protocol, leave a marker for post-commit so
  # the final commit SHA is recorded. A standalone copy records immediately.
  if [[ "${NEEDLE_PRE_COMMIT:-}" == 1 && "$COUNT_BYPASS" == true ]] &&
    type needle_mark_bypass >/dev/null 2>&1; then
    needle_mark_bypass "$lanes" "$pattern"
  else
    record_bypass "$pattern"
  fi
}

shell_join() {
  local value
  printf '%q' "$1"
  shift
  for value in "$@"; do
    printf ' %q' "$value"
  done
}

record_result() {
  local lane="$1"
  local name="$2"
  local status="$3"
  local exit_code="$4"
  local stdout="$5"
  local stderr="$6"

  RESULT_LANES+=("$lane")
  RESULT_NAMES+=("$name")
  RESULT_STATUS+=("$status")
  RESULT_EXIT_CODES+=("$exit_code")
  RESULT_STDOUT+=("$stdout")
  RESULT_STDERR+=("$stderr")
  TOTAL_CHECKS=$((TOTAL_CHECKS + 1))

  case "$status" in
    passed) TOTAL_PASSED=$((TOTAL_PASSED + 1)) ;;
    allowed_failure) TOTAL_ALLOWED_FAILURES=$((TOTAL_ALLOWED_FAILURES + 1)) ;;
    skipped) TOTAL_SKIPPED=$((TOTAL_SKIPPED + 1)) ;;
    failed|timeout) TOTAL_FAILED=$((TOTAL_FAILED + 1)) ;;
    *) log_error "Internal error: unknown result status '$status'"; return 1 ;;
  esac
}

execute_check() {
  local lane="$1"
  local name="$2"
  local command="$3"
  local timeout_seconds="$4"
  local allow_failure="$5"
  local stdout_file
  local stderr_file
  local exit_code
  local status
  local stdout
  local stderr

  log_info "Running: $name ($(shell_join "$command" "${CURRENT_ARGS[@]}"))"

  if [[ "$DRY_RUN" == true ]]; then
    log_warn "[dry run] would run with ${timeout_seconds}s timeout"
    record_result "$lane" "$name" skipped 0 '' ''
    return 0
  fi

  stdout_file="$(mktemp "${TMPDIR:-/tmp}/verification-runner-stdout.XXXXXX")"
  stderr_file="$(mktemp "${TMPDIR:-/tmp}/verification-runner-stderr.XXXXXX")"

  if timeout --kill-after=30s "$timeout_seconds" env \
    "${CURRENT_ENV[@]}" "$command" "${CURRENT_ARGS[@]}" \
    >"$stdout_file" 2>"$stderr_file"; then
    exit_code=0
  else
    exit_code=$?
  fi

  stdout="$(<"$stdout_file")"
  stderr="$(<"$stderr_file")"
  rm -f "$stdout_file" "$stderr_file"

  if ((exit_code == 0)); then
    status=passed
    log_success "$name passed"
  elif [[ "$allow_failure" == true ]]; then
    status=allowed_failure
    log_warn "$name failed (exit code: $exit_code), but failure is allowed"
  elif ((exit_code == 124)); then
    status=timeout
    log_error "$name timed out after ${timeout_seconds}s"
  else
    status=failed
    log_error "$name failed (exit code: $exit_code)"
  fi

  record_result "$lane" "$name" "$status" "$exit_code" "$stdout" "$stderr"

  if [[ "$VERBOSE" == true && ( -n "$stdout" || -n "$stderr" ) ]]; then
    printf '=== Output: ===\n'
    [[ -n "$stdout" ]] && printf '%s\n' "$stdout"
    printf '=== Errors: ===\n'
    [[ -n "$stderr" ]] && printf '%s\n' "$stderr"
  fi
  return 0
}

run_lane() {
  local lane="$1"
  local -a checks=()
  local check
  local name
  local command
  local timeout_seconds
  local allow_failure

  mapfile -t checks < <(jq -c --arg lane "$lane" '.[$lane] // [] | .[]' <<< "$CONFIG_JSON")
  if ((${#checks[@]} == 0)); then
    log_info "No checks configured for $lane"
    return 0
  fi

  log_info "Running $lane (${#checks[@]} checks)"
  for check in "${checks[@]}"; do
    name="$(jq -r '.name' <<< "$check")"
    command="$(jq -r '.command' <<< "$check")"
    timeout_seconds="$(jq -r '.timeout // 60' <<< "$check")"
    allow_failure="$(jq -r '.allow_failure // false' <<< "$check")"
    mapfile -t CURRENT_ARGS < <(jq -r '.args // [] | .[]' <<< "$check")
    mapfile -t CURRENT_ENV < <(jq -r '.environment // [] | .[]' <<< "$check")

    # execute_check always returns zero after recording a result, so the loop
    # remains explicit and easy to audit for the aggregate-failure contract.
    execute_check "$lane" "$name" "$command" "$timeout_seconds" "$allow_failure" || true
  done
}

generate_json_report() {
  local results='[]'
  local item
  local i
  local now

  for ((i = 0; i < TOTAL_CHECKS; i++)); do
    item="$(jq -cn \
      --arg lane "${RESULT_LANES[$i]}" \
      --arg name "${RESULT_NAMES[$i]}" \
      --arg status "${RESULT_STATUS[$i]}" \
      --arg stdout "${RESULT_STDOUT[$i]}" \
      --arg stderr "${RESULT_STDERR[$i]}" \
      --argjson exit_code "${RESULT_EXIT_CODES[$i]}" \
      '{lane: $lane, name: $name, status: $status, exit_code: $exit_code, stdout: $stdout, stderr: $stderr}')"
    results="$(jq -c --argjson item "$item" '. + [$item]' <<< "$results")"
  done

  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  jq -cn \
    --arg generated_at "$now" \
    --arg lane "$LANE" \
    --argjson checks "$TOTAL_CHECKS" \
    --argjson passed "$TOTAL_PASSED" \
    --argjson allowed_failures "$TOTAL_ALLOWED_FAILURES" \
    --argjson skipped "$TOTAL_SKIPPED" \
    --argjson failed "$TOTAL_FAILED" \
    --argjson results "$results" \
    '{schema_version: 1, generated_at: $generated_at, lane: $lane,
      total_checks: $checks, passed: $passed, failed: $failed,
      passed_checks: [$results[] | select(.status == "passed" or .status == "allowed_failure") | .name],
      totals: {checks: $checks, passed: $passed, allowed_failures: $allowed_failures,
               skipped: $skipped, failed: $failed},
      results: $results,
      failures: [$results[] | select(.status == "failed" or .status == "timeout")],
      failed_checks: [$results[] | select(.status == "failed" or .status == "timeout")
        | . + {output: ("STDOUT:\n" + .stdout + "\nSTDERR:\n" + .stderr)}]}'
}

print_report() {
  local i
  local first_line

  printf '\n=== Verification Summary ===\n'
  printf 'Lane: %s\n' "$LANE"
  printf 'Checks run: %d\n' "$TOTAL_CHECKS"
  printf 'Passed: %d\n' "$TOTAL_PASSED"
  printf 'Allowed failures: %d\n' "$TOTAL_ALLOWED_FAILURES"
  printf 'Skipped: %d\n' "$TOTAL_SKIPPED"
  printf 'Failed: %d\n' "$TOTAL_FAILED"

  if ((TOTAL_FAILED > 0)); then
    printf '\nFailed checks:\n\n'
    for ((i = 0; i < TOTAL_CHECKS; i++)); do
      [[ "${RESULT_STATUS[$i]}" == failed || "${RESULT_STATUS[$i]}" == timeout ]] || continue
      printf '  [%d] %s\n' "$((i + 1))" "${RESULT_NAMES[$i]}"
      printf '      Status: %s\n' "${RESULT_STATUS[$i]}"
      printf '      Exit code: %s\n' "${RESULT_EXIT_CODES[$i]}"
      if [[ -n "${RESULT_STDERR[$i]}" ]]; then
        printf '      Stderr:\n'
        printf '%s\n' "${RESULT_STDERR[$i]}" | sed 's/^/      | /'
      fi
      if [[ -n "${RESULT_STDOUT[$i]}" ]]; then
        if [[ "$VERBOSE" == true ]]; then
          printf '      Stdout:\n'
          printf '%s\n' "${RESULT_STDOUT[$i]}" | sed 's/^/      | /'
        else
          first_line="${RESULT_STDOUT[$i]}"
          [[ "$first_line" == *$'\n'* ]] && first_line="${first_line%%$'\n'*}"
          printf '      Stdout (first line): %s\n' "$first_line"
          printf '      (Use --verbose to see full output)\n'
        fi
      fi
      printf '\n'
    done
    printf '%s❌ Verification failed%s\n' "$RED" "$RESET"
    return 1
  fi

  printf '%s✓ All checks passed%s\n' "$GREEN" "$RESET"
  return 0
}

main() {
  local bypass_pattern
  local final_status
  local json_destination

  parse_arguments "$@"

  # The helper is optional: adopters can copy this runner by itself. When it
  # exists, use the shared NEEDLE event format and pre/post-commit state files.
  if [[ -r "$SCRIPT_DIR/bypass-detection.sh" ]]; then
    # shellcheck source=/dev/null
    source "$SCRIPT_DIR/bypass-detection.sh"
  fi

  bypass_pattern="$(detect_bypass_pattern || true)"
  if [[ -n "$bypass_pattern" ]]; then
    if ! handle_bypass "$bypass_pattern"; then
      log_error 'Unable to record verification bypass'
      exit 2
    fi
    exit 0
  fi

  if [[ -z "$CONFIG_PATH" ]]; then
    if ! CONFIG_PATH="$(detect_config_path)"; then
      log_error 'No verification configuration found'
      log_error 'Expected .verification/config.yaml or definition-of-done.yaml (or pass --config PATH)'
      exit 2
    fi
  elif [[ "$CONFIG_PATH" != /* ]]; then
    CONFIG_PATH="$REPO_ROOT/$CONFIG_PATH"
  fi

  if ! load_config "$CONFIG_PATH"; then
    exit 2
  fi

  log_info "Verification Runner (lane: $LANE)"
  [[ "$DRY_RUN" == true ]] && log_warn 'Dry run mode: no checks will execute'

  case "$LANE" in
    fast) run_lane fast_lane ;;
    slow) run_lane slow_lane ;;
    all)
      run_lane fast_lane
      run_lane slow_lane
      ;;
    *) log_error "Internal error: invalid lane '$LANE'"; exit 3 ;;
  esac

  if print_report; then
    final_status=0
  else
    final_status=1
  fi

  json_destination="$JSON_PATH"
  if [[ -z "$json_destination" && "${VERIFICATION_JSON_OUTPUT:-}" == true ]]; then
    json_destination="${VERIFICATION_JSON_PATH:-verification-results.json}"
  fi
  if [[ -n "$json_destination" ]]; then
    if ! mkdir -p "$(dirname "$json_destination")" || ! generate_json_report >"$json_destination"; then
      log_error "Unable to write JSON report: $json_destination"
      exit 2
    fi
    log_info "JSON report written to: $json_destination"
  fi

  if [[ "$COUNT_BYPASS" == true && "${NEEDLE_PRE_COMMIT:-}" == 1 ]] &&
    type needle_mark_verified >/dev/null 2>&1 && ((final_status == 0)); then
    if ! needle_mark_verified "$LANE"; then
      log_error 'Unable to record successful pre-commit verification state'
      exit 2
    fi
  fi

  exit "$final_status"
}

main "$@"
