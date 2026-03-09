#!/usr/bin/env bash
# NEEDLE Post-Execute File Conflict Reconciliation
# Strategy 2: Post-Execution Reconciliation (Reactive)
#
# Detects file conflicts AFTER agent execution by comparing changed files
# against the file lock registry. If a changed file is locked by a different
# bead, the change is rolled back and a dependency is added.
#
# This is the reactive complement to the proactive file-checkout hook
# (src/hooks/file-checkout.sh). It catches conflicts that slipped through
# (e.g., when hooks were not active or bypassed).
#
# Usage (from post-execute hook):
#   source "${NEEDLE_SRC_DIR:-...}/hooks/post-execute-reconcile.sh"
#   detect_file_conflicts
#
# Exit Codes (detect_file_conflicts):
#   0 - No conflicts detected
#   1 - Conflicts detected and rolled back; bead re-queued
#
# Environment Variables:
#   NEEDLE_BEAD_ID   - Current bead ID (required)
#   NEEDLE_WORKSPACE - Workspace path (defaults to pwd)
#   NEEDLE_LOCK_DIR  - Lock directory (default: /dev/shm/needle)

# ============================================================================
# Load Dependencies
# ============================================================================

# Resolve path to checkout module (lock/checkout.sh)
_POST_EXEC_RECONCILE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_LOCK_MODULE="${NEEDLE_LOCK_MODULE:-${_POST_EXEC_RECONCILE_DIR%/hooks}/lock/checkout.sh}"
NEEDLE_METRICS_MODULE="${NEEDLE_METRICS_MODULE:-${_POST_EXEC_RECONCILE_DIR%/hooks}/lock/metrics.sh}"

# Load lock module if not already loaded
if ! declare -f check_file &>/dev/null; then
    if [[ -f "$NEEDLE_LOCK_MODULE" ]]; then
        # shellcheck source=../lock/checkout.sh
        source "$NEEDLE_LOCK_MODULE"
    fi
fi

# Load metrics module if not already loaded
if ! declare -f _needle_metrics_record_event &>/dev/null; then
    if [[ -f "$NEEDLE_METRICS_MODULE" ]]; then
        # shellcheck source=../lock/metrics.sh
        source "$NEEDLE_METRICS_MODULE"
    fi
fi

# ============================================================================
# Logging Stubs (if parent modules not loaded)
# ============================================================================

if ! declare -f log_warn &>/dev/null; then
    log_warn()  { echo "[WARN] $*"  >&2; }
fi
if ! declare -f log_error &>/dev/null; then
    log_error() { echo "[ERROR] $*" >&2; }
fi
if ! declare -f log_info &>/dev/null; then
    log_info()  { echo "[INFO] $*"  >&2; }
fi

# ============================================================================
# Core: detect_file_conflicts
# ============================================================================

# Detect and roll back file conflicts after agent execution.
#
# For each file changed since HEAD:
#   1. Check if the file is locked by a different bead
#   2. If so, roll back the file to HEAD
#   3. Add a dependency on the locking bead
#   4. Emit a file.conflict.missed metric event
#
# At the end, if any conflicts were found:
#   - Re-queue the current bead (set status back to open)
#   - Return 1
#
# Usage: detect_file_conflicts
# Returns: 0 = no conflicts; 1 = conflicts rolled back
detect_file_conflicts() {
    local bead_id="${NEEDLE_BEAD_ID:-}"
    local workspace="${NEEDLE_WORKSPACE:-$(pwd)}"
    local conflicts=0

    # Require bead ID
    if [[ -z "$bead_id" ]]; then
        log_warn "detect_file_conflicts: NEEDLE_BEAD_ID not set, skipping reconciliation"
        return 0
    fi

    # Must be in a git repository
    if ! git -C "$workspace" rev-parse --git-dir &>/dev/null; then
        log_warn "detect_file_conflicts: workspace is not a git repository: $workspace"
        return 0
    fi

    # Get files changed since HEAD
    local changed_files
    changed_files=$(git -C "$workspace" diff --name-only HEAD 2>/dev/null || true)

    if [[ -z "$changed_files" ]]; then
        return 0
    fi

    log_info "detect_file_conflicts: checking ${#changed_files} changed file(s) for conflicts"

    while IFS= read -r rel_file; do
        [[ -z "$rel_file" ]] && continue

        # Resolve to absolute path
        local abs_file="${workspace}/${rel_file}"

        # Check if check_file function is available (lock module loaded)
        if ! declare -f check_file &>/dev/null; then
            log_warn "detect_file_conflicts: lock module not loaded, cannot check $rel_file"
            continue
        fi

        # Query lock registry
        local lock_info
        lock_info=$(check_file "$abs_file" 2>/dev/null) || true

        # lock_info is non-empty only when the file is locked
        if [[ -z "$lock_info" ]]; then
            continue
        fi

        # Determine which bead holds the lock
        local blocking_bead
        if command -v jq &>/dev/null; then
            blocking_bead=$(echo "$lock_info" | jq -r '.bead // empty' 2>/dev/null || true)
        else
            blocking_bead=$(echo "$lock_info" | grep -oE '"bead":"[^"]*"' | sed 's/.*:"\([^"]*\)".*/\1/' || true)
        fi

        [[ -z "$blocking_bead" ]] && continue

        # Only rollback if locked by a *different* bead
        if [[ "$blocking_bead" == "$bead_id" ]]; then
            continue
        fi

        log_warn "CONFLICT: $rel_file was changed but is locked by $blocking_bead"

        # Roll back the conflicting file to HEAD
        if git -C "$workspace" checkout HEAD -- "$rel_file" 2>/dev/null; then
            log_info "Rolled back: $rel_file"
        else
            log_error "Failed to roll back: $rel_file (manual resolution required)"
        fi

        # Add dependency so this bead is re-queued after the blocker completes
        if command -v br &>/dev/null; then
            br dep add "$bead_id" "$blocking_bead" 2>/dev/null || true
        fi

        # Emit missed-conflict metric
        if declare -f _needle_metrics_record_event &>/dev/null; then
            _needle_metrics_record_event "conflict.missed" "$bead_id" "$abs_file" \
                "blocked_by=$blocking_bead" \
                "strategy=post_exec_rollback" || true
        fi

        conflicts=$(( conflicts + 1 ))

    done <<< "$changed_files"

    if (( conflicts > 0 )); then
        log_error "detect_file_conflicts: $conflicts file conflict(s) detected and rolled back"

        # Re-queue this bead so it is picked up again after blockers resolve
        if command -v br &>/dev/null; then
            br update "$bead_id" --status open 2>/dev/null || true
        fi

        return 1
    fi

    return 0
}
