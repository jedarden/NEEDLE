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

# ── Create remote workspace with a bead ──────────────────────────────────────────

# Creates a beads workspace and adds a single bead to it.
# Useful for testing Explore strand that searches remote workspaces.
#
# Usage:
#   create_remote_workspace_with_bead <workspace_path> <bead_id_var> [title] [description]
#
# Arguments:
#   workspace_path - Directory path for the workspace (will be created)
#   bead_id_var    - Variable name to store the created bead ID
#   title          - Optional bead title (default: "Remote task")
#   description    - Optional bead description (default: "Remote workspace task")
#
# Environment:
#   BR_BIN - path to br CLI (required)
#
# Example:
#   create_remote_workspace_with_bead "$TMPBASE/remote_ws" REMOTE_BEAD_ID \
#       "Create DONE file" "Create a file called DONE in workspace root"

create_remote_workspace_with_bead() {
    local workspace_path="${1:?workspace_path required}"
    local bead_id_var="${2:?bead_id_var required}"
    local title="${3:-Remote task}"
    local description="${4:-Remote workspace task}"

    if [ -z "${BR_BIN:-}" ]; then
        echo "FATAL: BR_BIN not set. Set it before calling create_remote_workspace_with_bead."
        return 1
    fi

    # Create and initialize workspace
    mkdir -p "$workspace_path"
    (cd "$workspace_path" && "$BR_BIN" init 2>&1) || {
        echo "FATAL: br init failed for remote workspace at $workspace_path"
        return 1
    }

    # Create the bead (with retry on transient sync issues)
    local bead_id
    bead_id="$(cd "$workspace_path" && "$BR_BIN" create \
        --title "$title" \
        --description "$description" \
        --silent 2>/dev/null)" || {
        # Retry after sync flush on failure
        (cd "$workspace_path" && "$BR_BIN" sync --flush-only 2>/dev/null) || true
        bead_id="$(cd "$workspace_path" && "$BR_BIN" create \
            --title "$title" \
            --description "$description" \
            --silent)" || {
            echo "FATAL: br create failed in $workspace_path"
            return 1
        }
    }

    # Export bead ID to caller's variable name
    eval "$bead_id_var=\"\$bead_id\""
}

# ── Create home workspace (empty, no beads) ───────────────────────────────────────

# Creates an empty beads workspace with no beads.
# Useful for testing strand waterfall where home workspace has no work,
# so Pluck/Mend return NoWork and the worker must progress to Explore.
#
# Usage:
#   create_home_workspace <workspace_path>
#
# Arguments:
#   workspace_path - Directory path for the workspace (will be created)
#
# Environment:
#   BR_BIN - path to br CLI (required)
#
# Example:
#   create_home_workspace "$SCENARIO_DIR/home_workspace"

create_home_workspace() {
    local workspace_path="${1:?workspace_path required}"

    if [ -z "${BR_BIN:-}" ]; then
        echo "FATAL: BR_BIN not set. Set it before calling create_home_workspace."
        return 1
    fi

    mkdir -p "$workspace_path"
    (cd "$workspace_path" && "$BR_BIN" init 2>&1) || {
        echo "FATAL: br init failed for home workspace at $workspace_path"
        return 1
    }
}
