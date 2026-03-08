#!/usr/bin/env bash
# ============================================================================
# NEEDLE Hook: post-task
# ============================================================================
#
# PURPOSE:
#   Runs AFTER a complete task lifecycle finishes (claim → execute → complete).
#   Use this hook for end-of-task summaries, team notifications, metrics
#   reporting, or triggering downstream workflows.
#
# WHEN CALLED:
#   After the bead has been fully processed and marked as complete or failed.
#   This is the final hook in the lifecycle, running after post-complete or
#   on-failure/on-quarantine hooks.
#
# EXIT CODES:
#   0 - Success: Task lifecycle is complete
#   1 - Warning: Log warning (task is already done, warning is informational)
#   2 - Abort: Not applicable (task already finished, logged as error)
#   3 - Skip: Skip remaining post-task hooks
#
# ============================================================================
# AVAILABLE ENVIRONMENT VARIABLES
# ============================================================================
#
# NEEDLE_HOOK          - Name of this hook ("post_task")
# NEEDLE_BEAD_ID       - ID of the completed bead
# NEEDLE_BEAD_TITLE    - Title of the bead
# NEEDLE_BEAD_PRIORITY - Priority level (0=critical, 1=high, 2=normal, 3=low, 4=backlog)
# NEEDLE_BEAD_TYPE     - Type of bead (task, bug, feature, etc.)
# NEEDLE_BEAD_LABELS   - Comma-separated labels
# NEEDLE_WORKSPACE     - Path to workspace
# NEEDLE_SESSION       - Worker session ID
# NEEDLE_PID           - Process ID
# NEEDLE_WORKER        - Worker identifier
# NEEDLE_EXIT_CODE     - Final exit code (0=success, non-zero=failure)
# NEEDLE_DURATION_MS   - Total execution duration in milliseconds
# NEEDLE_OUTPUT_FILE   - Path to output file (if captured)
# NEEDLE_FILES_CHANGED - Number of files changed
# NEEDLE_LINES_ADDED   - Lines added
# NEEDLE_LINES_REMOVED - Lines removed
#
# ============================================================================
# EXAMPLE USE CASES
# ============================================================================
#
# 1. Send task completion summary to team channel
# 2. Update external project management tools (Jira, Linear, etc.)
# 3. Record task metrics and cycle time analytics
# 4. Trigger CI/CD pipelines for completed work
# 5. Generate task completion reports
# 6. Clean up all task-related temporary resources
#
# ============================================================================

set -euo pipefail

# ============================================================================
# POST-TASK EXAMPLES (Uncomment to enable)
# ============================================================================

echo "post-task hook called for bead: ${NEEDLE_BEAD_ID:-unknown}"
echo "  Title: ${NEEDLE_BEAD_TITLE:-}"
echo "  Exit code: ${NEEDLE_EXIT_CODE:-unknown}"
echo "  Duration: ${NEEDLE_DURATION_MS:-unknown}ms"
echo "  Worker: ${NEEDLE_WORKER:-unknown}"

# Determine task outcome
task_status="completed"
if [[ "${NEEDLE_EXIT_CODE:-0}" -ne 0 ]]; then
    task_status="failed"
fi

# ----------------------------------------------------------------------------
# Example 1: Send task summary to Slack
# ----------------------------------------------------------------------------
# Uncomment and configure for Slack notifications:
#
# SLACK_WEBHOOK_URL="https://hooks.slack.com/services/YOUR/WEBHOOK/URL"
#
# # Format duration as human-readable
# duration_s=$(( ${NEEDLE_DURATION_MS:-0} / 1000 ))
# if [[ "$duration_s" -ge 3600 ]]; then
#     duration_human="$(( duration_s / 3600 ))h $(( (duration_s % 3600) / 60 ))m"
# elif [[ "$duration_s" -ge 60 ]]; then
#     duration_human="$(( duration_s / 60 ))m $(( duration_s % 60 ))s"
# else
#     duration_human="${duration_s}s"
# fi
#
# # Choose emoji based on outcome
# if [[ "$task_status" == "completed" ]]; then
#     emoji=":white_check_mark:"
#     color="#36a64f"
# else
#     emoji=":x:"
#     color="#ff0000"
# fi
#
# curl -s -X POST -H 'Content-type: application/json' \
#     --data "{
#         \"attachments\": [{
#             \"color\": \"$color\",
#             \"title\": \"${emoji} Task ${task_status}: ${NEEDLE_BEAD_TITLE:-}\",
#             \"fields\": [
#                 {\"title\": \"Bead\", \"value\": \"${NEEDLE_BEAD_ID:-}\", \"short\": true},
#                 {\"title\": \"Worker\", \"value\": \"${NEEDLE_WORKER:-}\", \"short\": true},
#                 {\"title\": \"Duration\", \"value\": \"$duration_human\", \"short\": true},
#                 {\"title\": \"Files Changed\", \"value\": \"${NEEDLE_FILES_CHANGED:-0}\", \"short\": true}
#             ],
#             \"footer\": \"NEEDLE\",
#             \"ts\": $(date +%s)
#         }]
#     }" \
#     "$SLACK_WEBHOOK_URL" > /dev/null 2>&1 || true

# ----------------------------------------------------------------------------
# Example 2: Update external project management tool
# ----------------------------------------------------------------------------
# Uncomment to update Jira/Linear when task completes:
#
# # Example: Update Linear issue status
# LINEAR_API_KEY="${LINEAR_API_KEY:-}"
# LINEAR_ISSUE_ID="${NEEDLE_BEAD_ID:-}"
#
# if [[ -n "$LINEAR_API_KEY" && -n "$LINEAR_ISSUE_ID" ]]; then
#     # Map NEEDLE status to Linear state
#     if [[ "$task_status" == "completed" ]]; then
#         linear_state="Done"
#     else
#         linear_state="Cancelled"
#     fi
#
#     curl -s -X POST \
#         -H "Authorization: $LINEAR_API_KEY" \
#         -H "Content-Type: application/json" \
#         -d "{
#             \"query\": \"mutation { issueUpdate(id: \\\"$LINEAR_ISSUE_ID\\\", input: { stateId: \\\"$linear_state\\\" }) { success } }\"
#         }" \
#         "https://api.linear.app/graphql" > /dev/null 2>&1 || true
# fi

# ----------------------------------------------------------------------------
# Example 3: Record task metrics for analytics
# ----------------------------------------------------------------------------
# Uncomment to log metrics to a local or remote store:
#
# METRICS_FILE="${NEEDLE_WORKSPACE:-.}/.needle/metrics.jsonl"
# mkdir -p "$(dirname "$METRICS_FILE")"
#
# # Append metrics as JSONL
# echo "{
#     \"event\": \"task_completed\",
#     \"bead_id\": \"${NEEDLE_BEAD_ID:-}\",
#     \"title\": \"${NEEDLE_BEAD_TITLE:-}\",
#     \"status\": \"$task_status\",
#     \"exit_code\": ${NEEDLE_EXIT_CODE:-0},
#     \"duration_ms\": ${NEEDLE_DURATION_MS:-0},
#     \"worker\": \"${NEEDLE_WORKER:-unknown}\",
#     \"priority\": ${NEEDLE_BEAD_PRIORITY:-3},
#     \"type\": \"${NEEDLE_BEAD_TYPE:-task}\",
#     \"files_changed\": ${NEEDLE_FILES_CHANGED:-0},
#     \"lines_added\": ${NEEDLE_LINES_ADDED:-0},
#     \"lines_removed\": ${NEEDLE_LINES_REMOVED:-0},
#     \"timestamp\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
# }" >> "$METRICS_FILE"

# ----------------------------------------------------------------------------
# Example 4: Trigger CI/CD pipeline for completed work
# ----------------------------------------------------------------------------
# Uncomment to trigger a GitHub Actions workflow:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
#
# if [[ "$task_status" == "completed" ]] && command -v gh > /dev/null 2>&1; then
#     repo=$(git remote get-url origin 2>/dev/null | sed 's/.*github.com[/:]//' | sed 's/.git$//')
#
#     if [[ -n "$repo" ]]; then
#         echo "Triggering CI pipeline for completed task..."
#         gh workflow run ci.yml \
#             --repo "$repo" \
#             --field "bead_id=${NEEDLE_BEAD_ID:-}" \
#             --field "trigger=task_completed" \
#             2>/dev/null || echo "Note: Could not trigger CI workflow"
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 5: Generate completion report
# ----------------------------------------------------------------------------
# Uncomment to write a task summary file:
#
# REPORTS_DIR="${NEEDLE_WORKSPACE:-.}/.needle/reports"
# mkdir -p "$REPORTS_DIR"
#
# report_file="$REPORTS_DIR/${NEEDLE_BEAD_ID:-unknown}-$(date +%Y%m%d%H%M%S).md"
#
# cat > "$report_file" << REPORT
# # Task Report: ${NEEDLE_BEAD_TITLE:-Unknown}
#
# - **Bead ID:** ${NEEDLE_BEAD_ID:-}
# - **Status:** $task_status
# - **Type:** ${NEEDLE_BEAD_TYPE:-task}
# - **Priority:** ${NEEDLE_BEAD_PRIORITY:-3}
# - **Worker:** ${NEEDLE_WORKER:-unknown}
# - **Duration:** $(( ${NEEDLE_DURATION_MS:-0} / 1000 ))s
# - **Timestamp:** $(date -u +%Y-%m-%dT%H:%M:%SZ)
#
# ## Changes
# - Files changed: ${NEEDLE_FILES_CHANGED:-0}
# - Lines added: ${NEEDLE_LINES_ADDED:-0}
# - Lines removed: ${NEEDLE_LINES_REMOVED:-0}
#
# ## Labels
# ${NEEDLE_BEAD_LABELS:-none}
# REPORT
#
# echo "Report written to: $report_file"

# ----------------------------------------------------------------------------
# Example 6: Clean up all task-related resources
# ----------------------------------------------------------------------------
# Uncomment to perform final cleanup:
#
# # Remove task-specific temp files
# rm -rf "/tmp/needle-${NEEDLE_BEAD_ID:-}" 2>/dev/null || true
#
# # Remove task-specific lock files
# rm -f "/dev/shm/needle-lock-${NEEDLE_BEAD_ID:-}"* 2>/dev/null || true
#
# # Remove scratch/notes files
# rm -f "${NEEDLE_WORKSPACE:-.}/.needle/scratch/${NEEDLE_BEAD_ID:-}"* 2>/dev/null || true

# ============================================================================
# Default: Log task completion
# ============================================================================
echo "Task lifecycle complete for bead: ${NEEDLE_BEAD_ID:-unknown} (status: $task_status)"
exit 0
