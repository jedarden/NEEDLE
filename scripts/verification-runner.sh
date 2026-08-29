#!/usr/bin/env bash
# Verification Runner - Configurable Definition of Done Execution
#
# A generic verification runner that loads checks from a configuration file
# and executes them with proper failure aggregation.
#
# Usage:
#   scripts/verification-runner.sh [--fast|--slow|--all] [--config PATH] [--help]
#
# Configuration:
#   By default, loads from:
#   - .verification/config.yaml
#   - definition-of-done.yaml
#   - Or path specified via --config
#
# Lanes:
#   - Fast: Quick checks (fmt, lint, typecheck)
#   - Slow: Full test suite
#   - All: Both fast and slow lanes
#
# Behavior:
#   - Aggregates all failures rather than aborting on first
#   - Returns non-zero if ANY check fails
#   - Outputs structured failure report
#
# Exit codes:
#   0 - All checks passed
#   1 - One or more checks failed
#   2 - Configuration error or missing config file
#   3 - Invalid arguments

set -euo pipefail

# Result storage
declare -a PASSED_CHECKS=()
declare -a FAILED_CHECKS=()
declare -a PASSED_NAMES=()
declare -a FAILED_NAMES=()
declare -a FAILED_EXIT_CODES=()
declare -a FAILED_OUTPUTS=()

# Global counters
TOTAL_PASSED=0
TOTAL_FAILED=0

# Script directory for path resolution
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "$SCRIPT_DIR/..")"
cd "$REPO_ROOT"

# Configuration
LANE="all"                              # Default to running all lanes
CONFIG_PATH=""                          # Will be auto-detected if not specified
VERBOSE=false                           # Verbose output
DRY_RUN=false                           # Show what would run without executing

# Color output (disable with NO_COLOR=1)
if [[ -t 1 && "${NO_COLOR:-}" != "1" ]]; then
  readonly RED='\033[0;31m'
  readonly GREEN='\033[0;32m'
  readonly YELLOW='\033[0;33m'
  readonly BLUE='\033[0;34m'
  readonly RESET='\033[0m'
else
  readonly RED=''
  readonly GREEN=''
  readonly YELLOW=''
  readonly BLUE=''
  readonly RESET=''
fi

#######################################
# Print usage information
#######################################
usage() {
  cat <<'EOF'
Usage: scripts/verification-runner.sh [OPTIONS]

Verification Runner - Execute definition-of-done checks from configuration

OPTIONS:
  --fast              Run fast lane only (quick checks)
  --slow              Run slow lane only (test suite)
  --all               Run both fast and slow lanes (default)
  --config PATH       Load configuration from specified path
  --verbose           Show detailed output from each check
  --dry-run           Show what would run without executing
  --help              Show this help message

CONFIGURATION:
  By default, the runner looks for configuration in this order:
  1. .verification/config.yaml
  2. definition-of-done.yaml
  3. Custom path via --config

  Configuration format (YAML):
  ```yaml
  version: "1.0"

  fast_lane:
    - name: "Format check"
      command: "cargo"
      args: ["fmt", "--check"]
      timeout: 30

    - name: "Linting"
      command: "cargo"
      args: ["clippy", "--all-targets", "--", "-D", "warnings"]
      timeout: 60

  slow_lane:
    - name: "Unit tests"
      command: "cargo"
      args: ["test", "--lib"]
      timeout: 900
  ```

EXAMPLES:
  # Run all lanes with default config
  ./scripts/verification-runner.sh

  # Run fast lane only
  ./scripts/verification-runner.sh --fast

  # Use custom config file
  ./scripts/verification-runner.sh --config .my-verification.yaml

  # Dry run to see what would execute
  ./scripts/verification-runner.sh --dry-run --verbose

EXIT CODES:
  0 - All checks passed
  1 - One or more checks failed
  2 - Configuration error or missing config file
  3 - Invalid arguments

For more information, see: docs/verification-runner.md
EOF
}

#######################################
# Log messages with timestamp
#######################################
log_info() {
  echo -e "${BLUE}[$(date -u +%H:%M:%S)]${RESET} $*"
}

log_success() {
  echo -e "${GREEN}✓${RESET} $*"
}

log_error() {
  echo -e "${RED}✗${RESET} $*" >&2
}

log_warn() {
  echo -e "${YELLOW}⚠${RESET} $*" >&2
}

#######################################
# Detect configuration file location
#######################################
detect_config_path() {
  local candidates=(
    ".verification/config.yaml"
    "definition-of-done.yaml"
    ".verification/config.yml"
    "definition-of-done.yml"
  )

  for candidate in "${candidates[@]}"; do
    if [[ -f "$REPO_ROOT/$candidate" ]]; then
      echo "$REPO_ROOT/$candidate"
      return 0
    fi
  done

  return 1
}

#######################################
# Parse command-line arguments
#######################################
parse_arguments() {
  while [[ $# -gt 0 ]]; do
    case $1 in
      --fast)
        LANE="fast"
        shift
        ;;
      --slow)
        LANE="slow"
        shift
        ;;
      --all)
        LANE="all"
        shift
        ;;
      --config)
        if [[ -z "${2:-}" ]]; then
          log_error "Option --config requires an argument"
          usage
          exit 3
        fi
        CONFIG_PATH="$2"
        shift 2
        ;;
      --verbose)
        VERBOSE=true
        shift
        ;;
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        log_error "Unknown argument: $1"
        usage
        exit 3
        ;;
    esac
  done
}

#######################################
# Load configuration file
#######################################
load_config() {
  local config_file="$1"

  if [[ ! -f "$config_file" ]]; then
    log_error "Configuration file not found: $config_file"
    return 1
  fi

  # Check if yq is available for YAML parsing
  if ! command -v yq &>/dev/null; then
    log_error "yq is required for YAML parsing but not found in PATH"
    log_error "Install: https://github.com/mikefarah/yq"
    return 1
  fi

  # Validate basic YAML structure
  if ! yq eval '.version' "$config_file" &>/dev/null; then
    log_error "Invalid configuration format (missing or invalid 'version' field)"
    return 1
  fi

  log_info "Loaded configuration from: $config_file"
  return 0
}

#######################################
# Parse checks from YAML for a given lane
#######################################
parse_checks() {
  local config_file="$1"
  local lane="$2"
  local check_count

  # Get the number of checks in this lane
  check_count=$(yq eval ".${lane} | length" "$config_file" 2>/dev/null || echo "0")

  if [[ "$check_count" -eq 0 ]]; then
    log_warn "No checks found in $lane lane"
    return 0
  fi

  echo "$check_count"
}

#######################################
# Get check configuration by index
#######################################
get_check_config() {
  local config_file="$1"
  local lane="$2"
  local index="$3"
  local field="$4"

  yq eval ".${lane}[$index].${field}" "$config_file" 2>/dev/null || echo ""
}

#######################################
# Execute a single check with timeout
#######################################
execute_check() {
  local name="$1"
  local command="$2"
  local args="$3"
  local timeout="$4"
  local allow_failure="${5:-false}"
  local description="${6:-}"
  local environment="${7:-}"

  log_info "Running: $name..."

  if [[ "$DRY_RUN" == "true" ]]; then
    log_warn "[Dry Run] Would execute: $command $args (timeout: ${timeout}s)"
    PASSED_CHECKS+=("$name")
    PASSED_NAMES+=("$name")
    TOTAL_PASSED=$((TOTAL_PASSED + 1))
    return 0
  fi

  # Build environment variable exports if provided
  local env_setup=""
  if [[ -n "$environment" ]]; then
    # Parse environment variables from YAML format
    # Expected format from yq: "KEY1=value1\nKEY2=value2"
    while IFS= read -r env_line; do
      if [[ -n "$env_line" ]]; then
        env_setup="$env_setup export $env_line;"
      fi
    done <<< "$environment"
  fi

  # Create temp files for output capture
  local stdout_file
  local stderr_file
  stdout_file=$(mktemp)
  stderr_file=$(mktemp)

  # Ensure temp files are cleaned up
  trap 'rm -f "${stdout_file:-}" "${stderr_file:-}"' RETURN

  # Execute command with timeout
  # Use bash -c to properly handle environment variables and command arguments
  local cmd="$env_setup $command $args"
  local exit_code

  # Run with timeout, capturing output
  timeout "$timeout" bash -c "$cmd" </dev/null >"$stdout_file" 2>"$stderr_file" || exit_code=$?

  # timeout command returns 124 on timeout
  if [[ ${exit_code:-0} -eq 124 ]]; then
    log_error "✗ $name failed (timeout after ${timeout}s)"
    FAILED_CHECKS+=("$name")
    FAILED_NAMES+=("$name")
    FAILED_EXIT_CODES+=("124")
    FAILED_OUTPUTS+=("Timeout after ${timeout}s")
    TOTAL_FAILED=$((TOTAL_FAILED + 1))

    if [[ "$VERBOSE" == "true" ]]; then
      echo "=== Output (captured before timeout) ==="
      cat "$stdout_file"
      echo "=== Errors ==="
      cat "$stderr_file"
    fi
    return 1
  fi

  # Check exit code
  if [[ ${exit_code:-0} -ne 0 ]]; then
    local output
    output=$(cat "$stderr_file")

    if [[ "$allow_failure" == "true" ]]; then
      log_warn "⚠ $name failed (exit code: ${exit_code}) but failure is allowed"
      PASSED_CHECKS+=("$name")
      PASSED_NAMES+=("$name")
      TOTAL_PASSED=$((TOTAL_PASSED + 1))
    else
      log_error "✗ $name failed (exit code: ${exit_code})"

      FAILED_CHECKS+=("$name")
      FAILED_NAMES+=("$name")
      FAILED_EXIT_CODES+=("$exit_code")
      FAILED_OUTPUTS+=("$output")
      TOTAL_FAILED=$((TOTAL_FAILED + 1))

      if [[ "$VERBOSE" == "true" ]]; then
        echo "=== Output ==="
        cat "$stdout_file"
        echo "=== Errors ==="
        cat "$stderr_file"
      fi
    fi
    return 1
  fi

  log_success "✓ $name passed"
  PASSED_CHECKS+=("$name")
  PASSED_NAMES+=("$name")
  TOTAL_PASSED=$((TOTAL_PASSED + 1))

  if [[ "$VERBOSE" == "true" ]]; then
    echo "=== Output ==="
    cat "$stdout_file"
  fi

  return 0
}

#######################################
# Run all checks in a lane
#######################################
run_lane() {
  local config_file="$1"
  local lane="$2"

  local check_count
  check_count=$(parse_checks "$config_file" "$lane")

  if [[ "$check_count" -eq 0 ]]; then
    log_info "No checks to run in $lane lane"
    return 0
  fi

  log_info "Running $lane lane ($check_count checks)..."

  for ((i = 0; i < check_count; i++)); do
    local name
    local command
    local args
    local timeout
    local allow_failure
    local description
    local environment

    name=$(get_check_config "$config_file" "$lane" "$i" "name")
    command=$(get_check_config "$config_file" "$lane" "$i" "command")
    args=$(get_check_config "$config_file" "$lane" "$i" "args" | jq -r 'join(" ")' 2>/dev/null || echo "")
    timeout=$(get_check_config "$config_file" "$lane" "$i" "timeout")
    allow_failure=$(get_check_config "$config_file" "$lane" "$i" "allow_failure" 2>/dev/null || echo "false")
    description=$(get_check_config "$config_file" "$lane" "$i" "description" 2>/dev/null || echo "")
    environment=$(get_check_config "$config_file" "$lane" "$i" "environment" 2>/dev/null || echo "")

    # Validate required fields
    if [[ -z "$name" || -z "$command" ]]; then
      log_error "Check at index $i is missing required fields (name or command)"
      continue
    fi

    # Set default timeout if not specified
    timeout=${timeout:-60}

    # Convert YAML boolean to bash boolean
    allow_failure=$( [[ "$allow_failure" == "true" ]] && echo "true" || echo "false" )

    # Execute the check (continue even if it fails)
    execute_check "$name" "$command" "$args" "$timeout" "$allow_failure" "$description" "$environment" || true
  done

  log_info "Finished $lane lane"
}

#######################################
# Generate and display summary report
#######################################
generate_report() {
  local total_checks
  local total_failed
  local total_passed

  total_checks=$((TOTAL_PASSED + TOTAL_FAILED))
  total_failed=$TOTAL_FAILED
  total_passed=$TOTAL_PASSED

  echo ""
  echo "=== Verification Summary ==="
  echo "Lane: $LANE"
  echo "Checks run: $total_checks"
  echo "Passed: $total_passed"
  echo "Failed: $total_failed"
  echo ""

  if [[ $total_failed -gt 0 ]]; then
    echo "Failed checks:"
    for i in $(seq 0 $(($total_failed - 1))); do
      local name="${FAILED_NAMES[$i]}"
      local exit_code="${FAILED_EXIT_CODES[$i]}"
      echo "  - $name: exit code $exit_code"

      if [[ "$VERBOSE" == "true" ]]; then
        local output="${FAILED_OUTPUTS[$i]}"
        if [[ -n "$output" ]]; then
          echo "    Output: $output"
        fi
      fi
    done
    echo ""
    echo -e "${RED}❌ Verification failed${RESET}"
    return 1
  else
    echo -e "${GREEN}✓ All checks passed${RESET}"
    return 0
  fi
}

#######################################
# Generate JSON report for structured output
#######################################
generate_json_report() {
  local json_output
  json_output="{"
  json_output+="\"lane\":\"$LANE\","
  json_output+="\"total_checks\":$((TOTAL_PASSED + TOTAL_FAILED)),"
  json_output+="\"passed\":$TOTAL_PASSED,"
  json_output+="\"failed\":$TOTAL_FAILED,"

  # Add passed checks
  json_output+="\"passed_checks\":["
  local first=true
  for name in "${PASSED_NAMES[@]}"; do
    if [[ "$first" == "true" ]]; then
      first=false
    else
      json_output+=","
    fi
    json_output+="\"$name\""
  done
  json_output+="],"

  # Add failed checks with details
  json_output+="\"failed_checks\":["
  first=true
  for i in $(seq 0 $((TOTAL_FAILED - 1))); do
    if [[ "$first" == "true" ]]; then
      first=false
    else
      json_output+=","
    fi
    json_output+="{"
    json_output+="\"name\":\"${FAILED_NAMES[$i]}\","
    json_output+="\"exit_code\":${FAILED_EXIT_CODES[$i]},"
    json_output+="\"output\":$(echo "${FAILED_OUTPUTS[$i]}" | jq -Rs .)"
    json_output+="}"
  done
  json_output+="]"
  json_output+="}"

  echo "$json_output" | jq .
}

#######################################
# Main execution
#######################################
main() {
  # Parse arguments
  parse_arguments "$@"

  # Detect or validate config path
  if [[ -z "$CONFIG_PATH" ]]; then
    if ! CONFIG_PATH=$(detect_config_path); then
      log_error "No configuration file found"
      log_error "Searched for: .verification/config.yaml, definition-of-done.yaml"
      log_error "Create a config file or specify --config PATH"
      usage
      exit 2
    fi
  fi

  # Load configuration
  if ! load_config "$CONFIG_PATH"; then
    exit 2
  fi

  # Show configuration summary
  log_info "Verification Runner"
  log_info "Configuration: $CONFIG_PATH"
  log_info "Lane: $LANE"

  if [[ "$DRY_RUN" == "true" ]]; then
    log_warn "Dry run mode - no checks will be executed"
  fi

  # Run checks based on selected lane
  case "$LANE" in
    fast)
      run_lane "$CONFIG_PATH" "fast_lane"
      ;;
    slow)
      run_lane "$CONFIG_PATH" "slow_lane"
      ;;
    all)
      run_lane "$CONFIG_PATH" "fast_lane"
      run_lane "$CONFIG_PATH" "slow_lane"
      ;;
  esac

  # Generate and display report
  local exit_code
  if generate_report; then
    exit_code=0
  else
    exit_code=1
  fi

  # Optionally generate JSON report
  if [[ "${VERIFICATION_JSON_OUTPUT:-}" == "true" ]]; then
    generate_json_report > "${VERIFICATION_JSON_PATH:-verification-results.json}"
    log_info "JSON report written to: ${VERIFICATION_JSON_PATH:-verification-results.json}"
  fi

  exit "$exit_code"
}

# Run main function with all arguments
main "$@"
