#!/usr/bin/env bash
# NEEDLE CLI Constants
# Global constants and version information

# Version information
NEEDLE_VERSION="0.1.0"
NEEDLE_VERSION_MAJOR=0
NEEDLE_VERSION_MINOR=1
NEEDLE_VERSION_PATCH=0

# Exit codes
NEEDLE_EXIT_SUCCESS=0
NEEDLE_EXIT_ERROR=1
NEEDLE_EXIT_USAGE=2
NEEDLE_EXIT_CONFIG=3
NEEDLE_EXIT_RUNTIME=4
NEEDLE_EXIT_DEPENDENCY=5
NEEDLE_EXIT_TIMEOUT=6
NEEDLE_EXIT_CANCELLED=130

# Default paths
NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"
NEEDLE_CONFIG_NAME="config.yaml"
NEEDLE_CONFIG_FILE="${NEEDLE_CONFIG_FILE:-$NEEDLE_HOME/$NEEDLE_CONFIG_NAME}"
NEEDLE_STATE_DIR="state"
NEEDLE_CACHE_DIR="cache"
NEEDLE_LOG_DIR="logs"

# Feature flags
NEEDLE_DEFAULT_VERBOSE=false
NEEDLE_DEFAULT_QUIET=false
NEEDLE_DEFAULT_COLOR=true

# Default timeout and interval values (in seconds)
NEEDLE_DEFAULT_TIMEOUT=300
NEEDLE_DEFAULT_INTERVAL=5
NEEDLE_DEFAULT_RETRY_COUNT=3
NEEDLE_DEFAULT_RETRY_DELAY=2

# NATO phonetic alphabet for readable IDs
NEEDLE_NATO_ALPHABET=(
    "alpha" "bravo" "charlie" "delta" "echo" "foxtrot" "golf" "hotel"
    "india" "juliet" "kilo" "lima" "mike" "november" "oscar" "papa"
    "quebec" "romeo" "sierra" "tango" "uniform" "victor" "whiskey"
    "xray" "yankee" "zulu"
)

# Available subcommands
NEEDLE_SUBCOMMANDS=(
    "init"
    "run"
    "list"
    "status"
    "config"
    "logs"
    "version"
    "upgrade"
    "rollback"
    "setup"
    "agents"
    "heartbeat"
    "attach"
    "stop"
    "test-agent"
    "help"
)
