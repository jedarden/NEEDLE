#!/usr/bin/env bash
# ============================================================================
# NEEDLE Hook: pre-commit
# ============================================================================
#
# PURPOSE:
#   Runs BEFORE a worker commits code changes. Use this hook to validate
#   code quality, run linters, check for secrets, or enforce commit policies.
#
# WHEN CALLED:
#   Just before the worker creates a git commit for the bead's changes.
#   The changes are staged but not yet committed.
#
# EXIT CODES:
#   0 - Success: Proceed with the commit
#   1 - Warning: Log warning but proceed with commit
#   2 - Abort: Do NOT commit, worker should fix issues first
#   3 - Skip: Skip remaining pre-commit hooks but still commit
#
# ============================================================================
# AVAILABLE ENVIRONMENT VARIABLES
# ============================================================================
#
# NEEDLE_HOOK          - Name of this hook ("pre_commit")
# NEEDLE_BEAD_ID       - ID of the bead being worked on
# NEEDLE_BEAD_TITLE    - Title of the bead
# NEEDLE_BEAD_PRIORITY - Priority level (0=critical, 1=high, 2=normal, 3=low, 4=backlog)
# NEEDLE_BEAD_TYPE     - Type of bead (task, bug, feature, etc.)
# NEEDLE_BEAD_LABELS   - Comma-separated list of labels
# NEEDLE_WORKSPACE     - Path to the workspace directory
# NEEDLE_SESSION       - Worker session ID
# NEEDLE_PID           - Current process ID
# NEEDLE_WORKER        - Worker identifier
# NEEDLE_AGENT         - Agent name (if set)
# NEEDLE_STRAND        - Strand ID (if set)
# NEEDLE_FILES_CHANGED - Number of files changed
# NEEDLE_LINES_ADDED   - Lines added
# NEEDLE_LINES_REMOVED - Lines removed
#
# ============================================================================
# EXAMPLE USE CASES
# ============================================================================
#
# 1. Run linters (shellcheck, eslint, ruff, clippy)
# 2. Check for secrets or credentials in staged files
# 3. Enforce commit message conventions
# 4. Validate file size limits
# 5. Run unit tests on changed files
# 6. Check for merge conflict markers
#
# ============================================================================

set -euo pipefail

# ============================================================================
# VALIDATION EXAMPLES (Uncomment to enable)
# ============================================================================

echo "pre-commit hook called for bead: ${NEEDLE_BEAD_ID:-unknown}"
echo "  Title: ${NEEDLE_BEAD_TITLE:-}"
echo "  Files changed: ${NEEDLE_FILES_CHANGED:-unknown}"

# ----------------------------------------------------------------------------
# Example 1: Check for secrets in staged files
# ----------------------------------------------------------------------------
# Uncomment to scan for common secret patterns:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
#
# SECRETS_PATTERNS=(
#     'AKIA[0-9A-Z]{16}'                    # AWS Access Key
#     'password\s*=\s*["\x27][^"\x27]+'     # Hardcoded passwords
#     'api[_-]?key\s*=\s*["\x27][^"\x27]+'  # API keys
#     'BEGIN (RSA|DSA|EC) PRIVATE KEY'       # Private keys
#     'ghp_[a-zA-Z0-9]{36}'                 # GitHub personal access tokens
#     'sk-[a-zA-Z0-9]{48}'                  # OpenAI API keys
# )
#
# staged_files=$(git diff --cached --name-only 2>/dev/null || true)
# found_secrets=false
#
# for file in $staged_files; do
#     [[ -f "$file" ]] || continue
#     for pattern in "${SECRETS_PATTERNS[@]}"; do
#         if grep -qEi "$pattern" "$file" 2>/dev/null; then
#             echo "BLOCKED: Possible secret found in $file (pattern: $pattern)"
#             found_secrets=true
#         fi
#     done
# done
#
# if [[ "$found_secrets" == "true" ]]; then
#     echo "Remove secrets before committing"
#     exit 2  # Abort commit
# fi

# ----------------------------------------------------------------------------
# Example 2: Run linters on staged files
# ----------------------------------------------------------------------------
# Uncomment to run language-specific linters:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
# staged_files=$(git diff --cached --name-only 2>/dev/null || true)
# lint_failed=false
#
# # ShellCheck for bash scripts
# if command -v shellcheck > /dev/null 2>&1; then
#     for file in $staged_files; do
#         if [[ "$file" == *.sh ]]; then
#             if ! shellcheck "$file" 2>/dev/null; then
#                 echo "ShellCheck failed: $file"
#                 lint_failed=true
#             fi
#         fi
#     done
# fi
#
# # Python linting with ruff
# if command -v ruff > /dev/null 2>&1; then
#     for file in $staged_files; do
#         if [[ "$file" == *.py ]]; then
#             if ! ruff check "$file" 2>/dev/null; then
#                 echo "Ruff failed: $file"
#                 lint_failed=true
#             fi
#         fi
#     done
# fi
#
# # JavaScript/TypeScript with eslint
# if command -v eslint > /dev/null 2>&1; then
#     for file in $staged_files; do
#         if [[ "$file" == *.js || "$file" == *.ts || "$file" == *.tsx ]]; then
#             if ! eslint "$file" 2>/dev/null; then
#                 echo "ESLint failed: $file"
#                 lint_failed=true
#             fi
#         fi
#     done
# fi
#
# if [[ "$lint_failed" == "true" ]]; then
#     echo "Fix lint errors before committing"
#     exit 2  # Abort commit
# fi

# ----------------------------------------------------------------------------
# Example 3: Check for merge conflict markers
# ----------------------------------------------------------------------------
# Uncomment to prevent committing unresolved conflicts:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
# staged_files=$(git diff --cached --name-only 2>/dev/null || true)
#
# for file in $staged_files; do
#     [[ -f "$file" ]] || continue
#     if grep -qE '^(<{7}|={7}|>{7})' "$file" 2>/dev/null; then
#         echo "BLOCKED: Merge conflict markers found in $file"
#         exit 2  # Abort commit
#     fi
# done

# ----------------------------------------------------------------------------
# Example 4: Enforce file size limits
# ----------------------------------------------------------------------------
# Uncomment to prevent committing large files:
#
# MAX_FILE_SIZE_KB=1024  # 1MB limit
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
# staged_files=$(git diff --cached --name-only 2>/dev/null || true)
#
# for file in $staged_files; do
#     [[ -f "$file" ]] || continue
#     file_size_kb=$(du -k "$file" 2>/dev/null | cut -f1)
#     if [[ "${file_size_kb:-0}" -gt "$MAX_FILE_SIZE_KB" ]]; then
#         echo "BLOCKED: File too large: $file (${file_size_kb}KB > ${MAX_FILE_SIZE_KB}KB)"
#         echo "Consider using Git LFS for large files"
#         exit 2  # Abort commit
#     fi
# done

# ----------------------------------------------------------------------------
# Example 5: Run tests on changed files
# ----------------------------------------------------------------------------
# Uncomment to run fast tests before committing:
#
# cd "${NEEDLE_WORKSPACE:-.}" 2>/dev/null || exit 0
#
# # Only run tests if test files were changed
# staged_files=$(git diff --cached --name-only 2>/dev/null || true)
# has_test_changes=false
#
# for file in $staged_files; do
#     if [[ "$file" == *test* || "$file" == *spec* ]]; then
#         has_test_changes=true
#         break
#     fi
# done
#
# if [[ "$has_test_changes" == "true" ]]; then
#     echo "Running quick tests..."
#     if [[ -f "package.json" ]] && command -v npm > /dev/null 2>&1; then
#         npm test -- --bail 2>/dev/null || { echo "Tests failed"; exit 2; }
#     elif [[ -f "Cargo.toml" ]] && command -v cargo > /dev/null 2>&1; then
#         cargo test 2>/dev/null || { echo "Tests failed"; exit 2; }
#     elif command -v pytest > /dev/null 2>&1; then
#         pytest -x --tb=short 2>/dev/null || { echo "Tests failed"; exit 2; }
#     fi
# fi

# ----------------------------------------------------------------------------
# Example 6: Validate commit message format
# ----------------------------------------------------------------------------
# Uncomment to enforce conventional commit format:
#
# # This checks if the commit message follows conventional commits
# # Note: commit message is not directly available in pre-commit;
# # use a commit-msg hook or check the bead title for conventions
# if [[ -n "${NEEDLE_BEAD_TITLE:-}" ]]; then
#     if ! echo "${NEEDLE_BEAD_TITLE}" | grep -qE '^(feat|fix|chore|docs|style|refactor|test|perf|ci|build)\b'; then
#         echo "Warning: Bead title doesn't follow conventional format"
#         exit 1  # Warning only
#     fi
# fi

# ============================================================================
# Default: Allow commit to proceed
# ============================================================================
echo "Pre-commit checks passed for bead: ${NEEDLE_BEAD_ID:-unknown}"
exit 0
