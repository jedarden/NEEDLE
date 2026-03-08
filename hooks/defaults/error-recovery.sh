#!/usr/bin/env bash
# ============================================================================
# NEEDLE Hook: error-recovery
# ============================================================================
#
# PURPOSE:
#   Runs when a bead execution fails, BEFORE the standard on-failure hook.
#   Use this hook to attempt automatic recovery from known error patterns.
#   If recovery succeeds (exit 0), the bead may be retried automatically
#   instead of being sent to quarantine.
#
# WHEN CALLED:
#   After execution fails but before on-failure handling. This hook has the
#   opportunity to fix the underlying issue so a retry can succeed.
#
# EXIT CODES:
#   0 - Recovery succeeded: Retry execution of the bead
#   1 - Warning: Recovery partially succeeded, retry but log warning
#   2 - Abort: Recovery failed, proceed directly to quarantine (skip retry)
#   3 - Skip: Skip remaining error-recovery hooks, use default failure handling
#
# ============================================================================
# AVAILABLE ENVIRONMENT VARIABLES
# ============================================================================
#
# NEEDLE_HOOK          - Name of this hook ("error_recovery")
# NEEDLE_BEAD_ID       - ID of the failed bead
# NEEDLE_BEAD_TITLE    - Title of the bead
# NEEDLE_BEAD_PRIORITY - Priority level (0=critical, 1=high, 2=normal, 3=low, 4=backlog)
# NEEDLE_BEAD_TYPE     - Type of bead (task, bug, feature, etc.)
# NEEDLE_BEAD_LABELS   - Comma-separated labels
# NEEDLE_WORKSPACE     - Path to workspace
# NEEDLE_SESSION       - Worker session ID
# NEEDLE_PID           - Process ID
# NEEDLE_WORKER        - Worker identifier
# NEEDLE_EXIT_CODE     - Exit code from execution (non-zero)
# NEEDLE_DURATION_MS   - Duration before failure
# NEEDLE_OUTPUT_FILE   - Path to output file with error details
#
# ============================================================================
# EXAMPLE USE CASES
# ============================================================================
#
# 1. Auto-fix dependency issues (npm install, pip install)
# 2. Clear corrupted caches and retry
# 3. Handle disk space issues by freeing resources
# 4. Restart crashed services needed for execution
# 5. Handle rate limits with backoff
# 6. Fix file permission issues
# 7. Reset git state after merge conflicts
#
# ============================================================================

set -euo pipefail

# ============================================================================
# RECOVERY EXAMPLES (Uncomment to enable)
# ============================================================================

echo "error-recovery hook called for bead: ${NEEDLE_BEAD_ID:-unknown}"
echo "  Title: ${NEEDLE_BEAD_TITLE:-}"
echo "  Exit code: ${NEEDLE_EXIT_CODE:-unknown}"

# Read error output for pattern matching
error_context=""
if [[ -n "${NEEDLE_OUTPUT_FILE:-}" ]] && [[ -f "${NEEDLE_OUTPUT_FILE:-}" ]]; then
    error_context=$(tail -200 "${NEEDLE_OUTPUT_FILE:-}" 2>/dev/null || echo "")
fi

# ----------------------------------------------------------------------------
# Example 1: Auto-fix missing dependencies
# ----------------------------------------------------------------------------
# Uncomment to automatically install missing dependencies:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 3
#
# # Node.js: detect missing modules
# if echo "$error_context" | grep -qi "cannot find module\|module not found\|ERR_MODULE_NOT_FOUND"; then
#     echo "Detected missing Node.js module - running npm install..."
#     if [[ -f "package.json" ]] && command -v npm > /dev/null 2>&1; then
#         npm install 2>/dev/null && {
#             echo "Dependencies installed successfully - retrying"
#             exit 0  # Recovery succeeded, retry
#         }
#     fi
# fi
#
# # Python: detect missing imports
# if echo "$error_context" | grep -qi "ModuleNotFoundError\|ImportError\|No module named"; then
#     module=$(echo "$error_context" | grep -oP "No module named '\K[^']+")
#     if [[ -n "$module" ]]; then
#         echo "Detected missing Python module: $module"
#         if [[ -f "requirements.txt" ]] && command -v pip > /dev/null 2>&1; then
#             pip install -r requirements.txt 2>/dev/null && {
#                 echo "Python dependencies installed - retrying"
#                 exit 0  # Recovery succeeded
#             }
#         fi
#     fi
# fi
#
# # Rust: detect missing build dependencies
# if echo "$error_context" | grep -qi "could not compile\|unresolved import"; then
#     echo "Detected Rust compilation error - running cargo fetch..."
#     if command -v cargo > /dev/null 2>&1; then
#         cargo fetch 2>/dev/null && {
#             echo "Cargo dependencies fetched - retrying"
#             exit 0
#         }
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 2: Clear corrupted caches
# ----------------------------------------------------------------------------
# Uncomment to handle cache corruption:
#
# if echo "$error_context" | grep -qi "cache.*corrupt\|integrity check\|EINTEGRITY\|checksum mismatch"; then
#     echo "Detected cache corruption - clearing caches..."
#
#     cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 3
#
#     # npm cache
#     if command -v npm > /dev/null 2>&1; then
#         npm cache clean --force 2>/dev/null || true
#         rm -rf node_modules/.cache 2>/dev/null || true
#     fi
#
#     # pip cache
#     if command -v pip > /dev/null 2>&1; then
#         pip cache purge 2>/dev/null || true
#     fi
#
#     # cargo cache
#     if command -v cargo > /dev/null 2>&1; then
#         cargo clean 2>/dev/null || true
#     fi
#
#     echo "Caches cleared - retrying"
#     exit 0  # Recovery succeeded
# fi

# ----------------------------------------------------------------------------
# Example 3: Handle disk space issues
# ----------------------------------------------------------------------------
# Uncomment to free disk space and retry:
#
# if echo "$error_context" | grep -qi "no space left\|disk full\|ENOSPC\|out of disk"; then
#     echo "Detected disk space issue - attempting cleanup..."
#
#     freed_kb=0
#
#     # Clean temp files older than 1 hour
#     find /tmp -maxdepth 2 -type f -mmin +60 -delete 2>/dev/null || true
#
#     # Clean old NEEDLE diagnostics
#     rm -rf /tmp/needle-diagnostics/* 2>/dev/null || true
#
#     # Clean package manager caches
#     if command -v npm > /dev/null 2>&1; then
#         npm cache clean --force 2>/dev/null || true
#     fi
#
#     # Clean docker images if available
#     if command -v docker > /dev/null 2>&1; then
#         docker system prune -f 2>/dev/null || true
#     fi
#
#     # Check if we freed enough space (at least 100MB)
#     available_kb=$(df -k "${NEEDLE_WORKSPACE:-/tmp}" 2>/dev/null | awk 'NR==2 {print $4}')
#     if [[ "${available_kb:-0}" -gt 102400 ]]; then
#         echo "Freed disk space (${available_kb}KB available) - retrying"
#         exit 0  # Recovery succeeded
#     else
#         echo "Could not free enough disk space"
#         exit 2  # Quarantine
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 4: Restart crashed services
# ----------------------------------------------------------------------------
# Uncomment to restart services needed for execution:
#
# # Database connection errors
# if echo "$error_context" | grep -qi "connection refused\|ECONNREFUSED\|could not connect"; then
#     echo "Detected connection error - checking services..."
#
#     # PostgreSQL
#     if echo "$error_context" | grep -qi "postgres\|5432"; then
#         echo "Attempting to restart PostgreSQL..."
#         if command -v pg_isready > /dev/null 2>&1; then
#             sudo systemctl restart postgresql 2>/dev/null || \
#                 pg_ctl restart -D /var/lib/postgresql/data 2>/dev/null || true
#             sleep 2
#             if pg_isready > /dev/null 2>&1; then
#                 echo "PostgreSQL restarted - retrying"
#                 exit 0
#             fi
#         fi
#     fi
#
#     # Redis
#     if echo "$error_context" | grep -qi "redis\|6379"; then
#         echo "Attempting to restart Redis..."
#         sudo systemctl restart redis 2>/dev/null || \
#             redis-server --daemonize yes 2>/dev/null || true
#         sleep 1
#         if redis-cli ping > /dev/null 2>&1; then
#             echo "Redis restarted - retrying"
#             exit 0
#         fi
#     fi
#
#     # Docker containers
#     if echo "$error_context" | grep -qi "docker\|container"; then
#         echo "Attempting to restart Docker containers..."
#         cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 3
#         if [[ -f "docker-compose.yml" ]] && command -v docker-compose > /dev/null 2>&1; then
#             docker-compose up -d 2>/dev/null && {
#                 sleep 3
#                 echo "Docker services restarted - retrying"
#                 exit 0
#             }
#         fi
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 5: Handle rate limits with backoff
# ----------------------------------------------------------------------------
# Uncomment to wait and retry on rate limiting:
#
# if echo "$error_context" | grep -qi "rate limit\|too many requests\|429\|throttl"; then
#     echo "Detected rate limiting - applying backoff..."
#
#     # Extract retry-after if available
#     retry_after=$(echo "$error_context" | grep -oP 'retry.after[:\s]+\K\d+' | head -1)
#     wait_seconds="${retry_after:-30}"
#
#     # Cap wait at 5 minutes
#     if [[ "$wait_seconds" -gt 300 ]]; then
#         wait_seconds=300
#     fi
#
#     echo "Waiting ${wait_seconds}s before retry..."
#     sleep "$wait_seconds"
#
#     echo "Backoff complete - retrying"
#     exit 0  # Recovery succeeded
# fi

# ----------------------------------------------------------------------------
# Example 6: Fix file permission issues
# ----------------------------------------------------------------------------
# Uncomment to fix common permission errors:
#
# if echo "$error_context" | grep -qi "permission denied\|EACCES"; then
#     echo "Detected permission error - attempting fix..."
#
#     cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 3
#
#     # Fix script permissions
#     find . -name "*.sh" -not -perm -u+x -exec chmod +x {} \; 2>/dev/null || true
#
#     # Fix node_modules binaries
#     if [[ -d "node_modules/.bin" ]]; then
#         chmod +x node_modules/.bin/* 2>/dev/null || true
#     fi
#
#     # Fix venv permissions
#     if [[ -d ".venv/bin" ]]; then
#         chmod +x .venv/bin/* 2>/dev/null || true
#     fi
#
#     echo "Permissions fixed - retrying"
#     exit 0  # Recovery succeeded
# fi

# ----------------------------------------------------------------------------
# Example 7: Reset git state after conflicts
# ----------------------------------------------------------------------------
# Uncomment to recover from git-related errors:
#
# if echo "$error_context" | grep -qi "merge conflict\|CONFLICT\|needs merge\|not possible because you have unmerged"; then
#     echo "Detected git conflict - attempting resolution..."
#
#     cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 3
#
#     if git rev-parse --git-dir > /dev/null 2>&1; then
#         # Abort any in-progress merge or rebase
#         git merge --abort 2>/dev/null || true
#         git rebase --abort 2>/dev/null || true
#
#         # Pull latest changes
#         git pull --rebase 2>/dev/null && {
#             echo "Git state resolved - retrying"
#             exit 0
#         }
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 8: Identify non-recoverable errors (go to quarantine)
# ----------------------------------------------------------------------------
# Uncomment to detect permanent failures:
#
# PERMANENT_PATTERNS=(
#     "syntax error"
#     "compilation failed"
#     "type error"
#     "undefined reference"
#     "segmentation fault"
#     "stack overflow"
#     "assertion failed"
# )
#
# for pattern in "${PERMANENT_PATTERNS[@]}"; do
#     if echo "$error_context" | grep -qi "$pattern"; then
#         echo "Detected permanent error ($pattern) - quarantining"
#         exit 2  # Abort - go directly to quarantine
#     fi
# done

# ============================================================================
# Default: No recovery attempted, use standard failure handling
# ============================================================================
echo "No automatic recovery available for bead: ${NEEDLE_BEAD_ID:-unknown}"
exit 3  # Skip - fall through to standard on-failure handling
