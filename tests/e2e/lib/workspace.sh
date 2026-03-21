#!/bin/bash
# E2E test library: workspace management
#
# Provides functions for creating isolated temp directories with .beads/ workspaces.
# Source this file from other E2E tests.
#
# Usage:
#   source "$SCRIPT_DIR/lib/workspace.sh"
#   setup_beads_workspace WORKSPACE_VAR
#
# Functions:
#   setup_beads_workspace <var_name>
#     Creates a temp directory, initializes .beads/, and sets cleanup trap.
#     The workspace path is stored in the named variable.
#     Sets TMPBASE and WORKSPACE globals for backward compatibility.
#
# Environment:
#   BR_BIN - path to br CLI (required, should be set by caller)

set -euo pipefail

# ── Workspace setup ─────────────────────────────────────────────────────────────

setup_beads_workspace() {
    local var_name="${1:-WORKSPACE}"

    if [ -z "${BR_BIN:-}" ]; then
        echo "FATAL: BR_BIN not set. Set it before calling setup_beads_workspace."
        return 1
    fi

    # Create isolated temp structure
    TMPBASE="$(mktemp -d)"
    WORKSPACE="$TMPBASE/workspace"
    FAKE_HOME="$TMPBASE/home"

    # Cleanup trap
    cleanup() {
        rm -rf "$TMPBASE"
    }
    trap cleanup EXIT

    # Isolate HOME for br discovery
    REAL_HOME="$HOME"
    export HOME="$FAKE_HOME"
    mkdir -p "$HOME"

    # Create and initialize workspace
    mkdir -p "$WORKSPACE"
    (cd "$WORKSPACE" && "$BR_BIN" init 2>&1) || {
        echo "FATAL: br init failed"
        return 1
    }

    # Export to caller's variable name
    eval "$var_name=\"\$WORKSPACE\""

    # Export globals for backward compatibility
    export TMPBASE WORKSPACE FAKE_HOME REAL_HOME
}

# ── Verify workspace has .beads ────────────────────────────────────────────────

verify_beads_workspace() {
    local workspace="${1:-$WORKSPACE}"

    if [ ! -d "$workspace/.beads" ]; then
        echo "FATAL: No .beads directory in $workspace"
        return 1
    fi

    if [ ! -f "$workspace/.beads/issues.jsonl" ]; then
        echo "FATAL: No issues.jsonl in $workspace/.beads"
        return 1
    fi

    return 0
}
