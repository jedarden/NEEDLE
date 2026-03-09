#!/usr/bin/env bash
# ============================================================================
# NEEDLE Hook: post-execute
# ============================================================================
#
# PURPOSE:
#   Runs AFTER a worker finishes executing a bead. Use this hook to clean
#   up resources, collect artifacts, or log results.
#
# WHEN CALLED:
#   After the worker has completed execution (success or failure).
#   The exit code and duration are available for inspection.
#
# EXIT CODES:
#   0 - Success: Continue to completion workflow
#   1 - Warning: Log warning but continue
#   2 - Abort: Prevent marking bead as complete (if it succeeded)
#   3 - Skip: Skip remaining post-execute hooks
#
# ============================================================================
# AVAILABLE ENVIRONMENT VARIABLES
# ============================================================================
#
# NEEDLE_HOOK          - Name of this hook ("post_execute")
# NEEDLE_BEAD_ID       - ID of the executed bead
# NEEDLE_BEAD_TITLE    - Title of the bead
# NEEDLE_BEAD_PRIORITY - Priority level (0-4)
# NEEDLE_BEAD_TYPE     - Type of bead
# NEEDLE_BEAD_LABELS   - Comma-separated labels
# NEEDLE_WORKSPACE     - Path to workspace
# NEEDLE_SESSION       - Worker session ID
# NEEDLE_PID           - Process ID
# NEEDLE_WORKER        - Worker identifier
# NEEDLE_EXIT_CODE     - Exit code of the execution (0=success)
# NEEDLE_DURATION_MS   - Duration of execution in milliseconds
# NEEDLE_OUTPUT_FILE   - Path to output file (if captured)
#
# ============================================================================
# EXAMPLE USE CASES
# ============================================================================
#
# 1. Clean up temporary files and directories
# 2. Stop services started during pre-execute
# 3. Collect and archive build artifacts
# 4. Upload logs to external storage
# 5. Send execution metrics to monitoring
# 6. Restore environment to previous state
#
# ============================================================================

set -euo pipefail

# ============================================================================
# FILE CONFLICT RECONCILIATION (Strategy 2: Post-Execution Reactive)
# ============================================================================
#
# Detect and roll back any file changes that conflict with active file locks
# held by other beads. This is the safety net for missed conflicts.
#
# To ENABLE: uncomment the block below.
# Requires: NEEDLE_SRC_DIR to be set (or the reconcile module to be on PATH).
#
# RECONCILE_MODULE="${NEEDLE_SRC_DIR:-}/hooks/post-execute-reconcile.sh"
#
# if [[ -f "$RECONCILE_MODULE" ]]; then
#     source "$RECONCILE_MODULE"
#     if ! detect_file_conflicts; then
#         echo "File conflicts detected and rolled back — bead re-queued"
#         exit 2  # Abort completion, bead will be re-queued
#     fi
# fi

# ============================================================================
# CLEANUP EXAMPLES (Uncomment to enable)
# ============================================================================

echo "post-execute hook called for bead: ${NEEDLE_BEAD_ID:-unknown}"
echo "  Exit code: ${NEEDLE_EXIT_CODE:-unknown}"
echo "  Duration: ${NEEDLE_DURATION_MS:-unknown}ms"

# ----------------------------------------------------------------------------
# Example 1: Clean up temporary directories
# ----------------------------------------------------------------------------
# Uncomment to enable temp directory cleanup:
#
# TEMP_DIR="/tmp/needle-${NEEDLE_BEAD_ID:-}"
#
# if [[ -d "$TEMP_DIR" ]]; then
#     echo "Cleaning up temp directory: $TEMP_DIR"
#
#     # Optionally archive before deleting
#     if [[ "${NEEDLE_EXIT_CODE:-0}" -eq 0 ]]; then
#         ARCHIVE_DIR="${NEEDLE_WORKSPACE:-.}/.needle/artifacts"
#         mkdir -p "$ARCHIVE_DIR"
#         tar -czf "$ARCHIVE_DIR/${NEEDLE_BEAD_ID}-$(date +%Y%m%d%H%M%S).tar.gz" -C "$TEMP_DIR" . 2>/dev/null || true
#         echo "Archived temp files to: $ARCHIVE_DIR"
#     fi
#
#     rm -rf "$TEMP_DIR"
#     echo "Temp directory removed"
# fi

# ----------------------------------------------------------------------------
# Example 2: Stop services started during execution
# ----------------------------------------------------------------------------
# Uncomment to stop Docker containers:
#
# if command -v docker > /dev/null 2>&1; then
#     # Stop any test containers we started
#     container_name="test-db-${NEEDLE_BEAD_ID:-}"
#
#     if docker ps -q -f name="$container_name" > /dev/null 2>&1; then
#         echo "Stopping container: $container_name"
#         docker stop "$container_name" > /dev/null 2>&1 || true
#         docker rm "$container_name" > /dev/null 2>&1 || true
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 3: Collect and archive logs
# ----------------------------------------------------------------------------
# Uncomment to collect logs:
#
# LOG_DIR="${NEEDLE_WORKSPACE:-.}/.needle/logs"
# mkdir -p "$LOG_DIR"
#
# # If there's an output file, archive it
# if [[ -n "${NEEDLE_OUTPUT_FILE:-}" ]] && [[ -f "${NEEDLE_OUTPUT_FILE:-}" ]]; then
#     cp "${NEEDLE_OUTPUT_FILE:-}" "$LOG_DIR/${NEEDLE_BEAD_ID}-output.log"
#     echo "Archived output to: $LOG_DIR/${NEEDLE_BEAD_ID}-output.log"
# fi
#
# # Collect system logs if available
# if command -v journalctl > /dev/null 2>&1; then
#     journalctl --since "5 minutes ago" > "$LOG_DIR/${NEEDLE_BEAD_ID}-system.log" 2>/dev/null || true
# fi

# ----------------------------------------------------------------------------
# Example 4: Upload artifacts to cloud storage
# ----------------------------------------------------------------------------
# Uncomment to upload to S3/GCS/etc:
#
# ARTIFACT_DIR="${NEEDLE_WORKSPACE:-.}/artifacts"
#
# if [[ -d "$ARTIFACT_DIR" ]] && [[ -n "$(ls -A "$ARTIFACT_DIR" 2>/dev/null)" ]]; then
#     BUCKET="your-bucket-name"
#     S3_PATH="s3://$BUCKET/artifacts/${NEEDLE_BEAD_ID}/$(date +%Y%m%d%H%M%S)"
#
#     if command -v aws > /dev/null 2>&1; then
#         echo "Uploading artifacts to $S3_PATH..."
#         aws s3 sync "$ARTIFACT_DIR" "$S3_PATH" --quiet 2>/dev/null || {
#             echo "Warning: Failed to upload artifacts"
#         }
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 5: Send execution metrics
# ----------------------------------------------------------------------------
# Uncomment to send metrics to monitoring system:
#
# if command -v curl > /dev/null 2>&1; then
#     METRICS_URL="https://metrics.example.com/api/v1/events"
#
#     curl -s -X POST \
#         -H "Content-Type: application/json" \
#         -d "{
#             \"event\": \"bead_executed\",
#             \"bead_id\": \"${NEEDLE_BEAD_ID:-}\",
#             \"exit_code\": ${NEEDLE_EXIT_CODE:-0},
#             \"duration_ms\": ${NEEDLE_DURATION_MS:-0},
#             \"worker\": \"${NEEDLE_WORKER:-unknown}\",
#             \"timestamp\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
#         }" \
#         "$METRICS_URL" > /dev/null 2>&1 || true
# fi

# ----------------------------------------------------------------------------
# Example 6: Restore environment state
# ----------------------------------------------------------------------------
# Uncomment to restore git state:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
#
# if git rev-parse --git-dir > /dev/null 2>&1; then
#     # Restore any stashed changes from pre-execute
#     if git stash list | grep -q "pre-execute stash for ${NEEDLE_BEAD_ID:-}"; then
#         echo "Restoring stashed changes..."
#         git stash pop 2>/dev/null || {
#             echo "Warning: Could not restore stashed changes"
#         }
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 7: Clean up node_modules if disk space is low
# ----------------------------------------------------------------------------
# Uncomment for conditional cleanup:
#
# available_kb=$(df -k "${NEEDLE_WORKSPACE:-/tmp}" | awk 'NR==2 {print $4}')
# min_disk_kb=$((512 * 1024))  # 512MB
#
# if [[ "$available_kb" -lt "$min_disk_kb" ]]; then
#     echo "Low disk space, cleaning up caches..."
#
#     # Clean npm cache
#     if command -v npm > /dev/null 2>&1; then
#         npm cache clean --force 2>/dev/null || true
#     fi
#
#     # Clean cargo cache
#     if command -v cargo > /dev/null 2>&1; then
#         cargo cache --autoclean 2>/dev/null || true
#     fi
# fi

# ============================================================================
# Default: Continue to completion workflow
# ============================================================================
echo "Post-execution cleanup complete for bead: ${NEEDLE_BEAD_ID:-unknown}"
exit 0
