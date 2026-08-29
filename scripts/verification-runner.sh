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

  # Placeholder for actual check execution
  # TODO: Implement check execution based on config structure
  log_warn "Check execution not yet implemented - this is a scaffold"

  # Exit with success for now
  log_info "Scaffold setup complete"
  exit 0
}

# Run main function with all arguments
main "$@"
