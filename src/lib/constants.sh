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

# Default paths
NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"
NEEDLE_CONFIG_FILE="config.yaml"
NEEDLE_STATE_DIR="state"
NEEDLE_CACHE_DIR="cache"
NEEDLE_LOG_DIR="logs"

# Feature flags
NEEDLE_DEFAULT_VERBOSE=false
NEEDLE_DEFAULT_QUIET=false
NEEDLE_DEFAULT_COLOR=true

# Available subcommands
NEEDLE_SUBCOMMANDS=(
    "init"
    "run"
    "list"
    "status"
    "config"
    "version"
    "help"
)
