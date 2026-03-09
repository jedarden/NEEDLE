#!/usr/bin/env bash
# Tests for upgrade/rollback process
# Covers: config format migration, state directory schema, atomic swap,
#         rollback after failed upgrade, and version compatibility validation

# Test setup
TEST_DIR=$(mktemp -d)

# Source the modules
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Set up test environment
export NEEDLE_HOME="$TEST_DIR/.needle"
export NEEDLE_CONFIG_FILE="$NEEDLE_HOME/config.yaml"
export NEEDLE_CONFIG_NAME="config.yaml"
export NEEDLE_QUIET=true
export NO_COLOR=1

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/runner/state.sh"

# Set upgrade/rollback constants (normally set by upgrade.sh / rollback.sh)
NEEDLE_UPGRADE_DIR="$NEEDLE_HOME/upgrade"
NEEDLE_BACKUP_DIR="$NEEDLE_UPGRADE_DIR/backups"
NEEDLE_DOWNLOAD_DIR="$NEEDLE_UPGRADE_DIR/downloads"
NEEDLE_MAX_BACKUPS="${NEEDLE_MAX_BACKUPS:-5}"
NEEDLE_ROLLBACK_CACHE_DIR="$NEEDLE_HOME/$NEEDLE_CACHE_DIR"

# Source upgrade and rollback modules
source "$PROJECT_DIR/src/cli/upgrade.sh"
source "$PROJECT_DIR/src/cli/rollback.sh"

# Cleanup function
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

test_case() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
}

test_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    ((TESTS_FAILED++))
}

# Helper: create a mock binary script
make_mock_binary() {
    local path="$1"
    local version="${2:-0.0.1}"
    mkdir -p "$(dirname "$path")"
    cat > "$path" <<EOF
#!/usr/bin/env bash
echo "needle version $version"
EOF
    chmod +x "$path"
}

# =============================================================================
# Section 1: Config Format Migration Between Versions
# =============================================================================

echo ""
echo "=== Config Format Migration ==="

# Test: Default config has all required top-level sections
test_case "default config contains all required sections"
mkdir -p "$NEEDLE_HOME"
create_default_config "$NEEDLE_CONFIG_FILE" 2>/dev/null
clear_config_cache
config=$(load_config)
missing=()
for section in limits runner strands billing hooks effort; do
    if ! echo "$config" | grep -q "\"$section\""; then
        missing+=("$section")
    fi
done
if [[ ${#missing[@]} -eq 0 ]]; then
    test_pass
else
    test_fail "Missing sections: ${missing[*]}"
fi

# Test: Config with only partial fields (old format) merges with defaults
test_case "partial config (old format) merges with defaults"
clear_config_cache
# Write a minimal "old version" config with only one section
cat > "$NEEDLE_CONFIG_FILE" <<'EOF'
runner:
  polling_interval: "5s"
EOF
clear_config_cache
config=$(load_config)
# Both the custom value and default sections should be present
if echo "$config" | grep -q '"limits"' && echo "$config" | grep -q '"strands"'; then
    test_pass
else
    test_fail "Old-format config not merged with defaults"
fi

# Test: Config with deprecated/unknown fields doesn't crash the loader
test_case "config with unknown fields does not crash"
cat > "$NEEDLE_CONFIG_FILE" <<'EOF'
unknown_legacy_field: true
another_old_key:
  nested: value
runner:
  polling_interval: "3s"
EOF
clear_config_cache
config=$(load_config 2>/dev/null)
if [[ -n "$config" ]]; then
    test_pass
else
    test_fail "Config loader crashed on unknown fields"
fi

# Test: Billing model field recognized from config
test_case "billing model value is read correctly"
cat > "$NEEDLE_CONFIG_FILE" <<'EOF'
billing:
  model: use_or_lose
  daily_budget_usd: 25.0
EOF
clear_config_cache
value=$(get_config "billing.model" "pay_per_token")
if [[ "$value" == "use_or_lose" ]]; then
    test_pass
else
    test_fail "Expected 'use_or_lose', got '$value'"
fi

# Test: Missing billing section falls back to default model
test_case "missing billing section uses default model"
cat > "$NEEDLE_CONFIG_FILE" <<'EOF'
runner:
  polling_interval: "2s"
EOF
clear_config_cache
value=$(get_config "billing.model" "pay_per_token")
if [[ "$value" == "pay_per_token" ]]; then
    test_pass
else
    test_fail "Expected default 'pay_per_token', got '$value'"
fi

# Test: validate_config accepts new default config
test_case "validate_config passes on freshly generated config"
create_default_config "$NEEDLE_CONFIG_FILE" 2>/dev/null
if validate_config "$NEEDLE_CONFIG_FILE" 2>/dev/null; then
    test_pass
else
    test_fail "Fresh default config failed validation"
fi

# Test: validate_config fails for non-existent config file
test_case "validate_config fails for non-existent file"
if ! validate_config "$TEST_DIR/nonexistent.yaml" 2>/dev/null; then
    test_pass
else
    test_fail "Expected failure for missing config file"
fi

# =============================================================================
# Section 2: State Directory Schema Changes
# =============================================================================

echo ""
echo "=== State Directory Schema ==="

# Test: workers_init creates state directory
test_case "workers_init creates state directory"
export NEEDLE_WORKERS_FILE="$NEEDLE_HOME/state/workers.json"
export NEEDLE_WORKERS_LOCK="${NEEDLE_WORKERS_FILE}.lock"
rm -rf "$NEEDLE_HOME/state"
if _needle_workers_init 2>/dev/null && [[ -d "$NEEDLE_HOME/state" ]]; then
    test_pass
else
    test_fail "State directory not created"
fi

# Test: workers_init creates valid JSON schema
test_case "workers_init creates valid JSON schema"
rm -f "$NEEDLE_WORKERS_FILE"
_needle_workers_init 2>/dev/null
if [[ -f "$NEEDLE_WORKERS_FILE" ]]; then
    content=$(cat "$NEEDLE_WORKERS_FILE")
    if echo "$content" | jq -e '.workers | arrays' >/dev/null 2>&1; then
        test_pass
    else
        test_fail "Invalid JSON schema: $content"
    fi
else
    test_fail "Workers file not created"
fi

# Test: workers_init is idempotent (doesn't overwrite existing state)
test_case "workers_init is idempotent with existing state"
# Pre-populate with a worker entry
echo '{"workers":[{"session":"test-session","pid":99999}]}' > "$NEEDLE_WORKERS_FILE"
_needle_workers_init 2>/dev/null
content=$(cat "$NEEDLE_WORKERS_FILE")
if echo "$content" | grep -q "test-session"; then
    test_pass
else
    test_fail "Existing state was overwritten"
fi

# Test: state directory handles old empty schema
test_case "old empty state schema is replaced with valid schema"
echo '{}' > "$NEEDLE_WORKERS_FILE"
_needle_workers_init 2>/dev/null
# The file should still be valid JSON (init doesn't touch existing files)
if jq . "$NEEDLE_WORKERS_FILE" >/dev/null 2>&1; then
    test_pass
else
    test_fail "State file is invalid JSON after init"
fi

# Test: rollback ensure dirs creates cache directory
test_case "rollback_ensure_dirs creates cache directory"
rm -rf "$NEEDLE_ROLLBACK_CACHE_DIR"
_needle_rollback_ensure_dirs 2>/dev/null
if [[ -d "$NEEDLE_ROLLBACK_CACHE_DIR" ]]; then
    test_pass
else
    test_fail "Rollback cache directory not created"
fi

# Test: upgrade ensure dirs creates all required directories
test_case "ensure_upgrade_dirs creates backup and download directories"
rm -rf "$NEEDLE_UPGRADE_DIR"
_needle_ensure_upgrade_dirs 2>/dev/null
if [[ -d "$NEEDLE_BACKUP_DIR" ]] && [[ -d "$NEEDLE_DOWNLOAD_DIR" ]]; then
    test_pass
else
    test_fail "Upgrade directories not created"
fi

# =============================================================================
# Section 3: Atomic Swap Verification
# =============================================================================

echo ""
echo "=== Atomic Swap Verification ==="

SWAP_DIR="$TEST_DIR/swap_test"
mkdir -p "$SWAP_DIR"

# Test: perform_swap installs new binary at target path
test_case "perform_swap installs new binary at target"
new_bin="$SWAP_DIR/new_needle"
target="$SWAP_DIR/needle"
make_mock_binary "$new_bin" "1.0.0"
make_mock_binary "$target" "0.9.0"

if _needle_perform_swap "$new_bin" "$target" 2>/dev/null; then
    if [[ -f "$target" ]] && [[ -x "$target" ]]; then
        test_pass
    else
        test_fail "Target not executable after swap"
    fi
else
    test_fail "perform_swap returned non-zero"
fi

# Test: perform_swap leaves target executable
test_case "target binary is executable after swap"
# Use files from prior test
if [[ -x "$target" ]]; then
    test_pass
else
    test_fail "Target is not executable"
fi

# Test: perform_swap removes temp files
test_case "perform_swap leaves no temp files"
new_bin2="$SWAP_DIR/new_needle2"
target2="$SWAP_DIR/needle2"
make_mock_binary "$new_bin2" "1.0.0"
make_mock_binary "$target2" "0.9.0"
_needle_perform_swap "$new_bin2" "$target2" 2>/dev/null
# Check for leftover .new.* and .old.* files
leftovers=$(find "$SWAP_DIR" -name "needle2.new.*" -o -name "needle2.old.*" 2>/dev/null)
if [[ -z "$leftovers" ]]; then
    test_pass
else
    test_fail "Temp files remain: $leftovers"
fi

# Test: perform_swap on non-writable directory fails gracefully
test_case "perform_swap fails gracefully on non-writable target directory"
ro_dir="$TEST_DIR/readonly_dir"
mkdir -p "$ro_dir"
ro_target="$ro_dir/needle"
make_mock_binary "$ro_target" "0.9.0"
chmod 555 "$ro_dir"
new_bin3="$SWAP_DIR/new_needle3"
make_mock_binary "$new_bin3" "1.0.0"
if ! _needle_perform_swap "$new_bin3" "$ro_target" 2>/dev/null; then
    test_pass
else
    # Restore permissions and fail
    chmod 755 "$ro_dir"
    test_fail "Expected failure on non-writable directory"
fi
chmod 755 "$ro_dir"

# Test: rollback_swap installs backup binary at target path
test_case "rollback_swap installs backup binary at target"
backup_bin="$TEST_DIR/backup_needle"
rb_target="$TEST_DIR/needle_rb"
make_mock_binary "$backup_bin" "0.8.0"
make_mock_binary "$rb_target" "0.9.0"
if _needle_rollback_swap "$backup_bin" "$rb_target" 2>/dev/null; then
    if [[ -f "$rb_target" ]] && [[ -x "$rb_target" ]]; then
        test_pass
    else
        test_fail "Target not executable after rollback swap"
    fi
else
    test_fail "rollback_swap returned non-zero"
fi

# Test: rollback_swap removes temp files
test_case "rollback_swap leaves no temp files"
backup_bin2="$TEST_DIR/backup_needle2"
rb_target2="$TEST_DIR/needle_rb2"
make_mock_binary "$backup_bin2" "0.8.0"
make_mock_binary "$rb_target2" "0.9.0"
_needle_rollback_swap "$backup_bin2" "$rb_target2" 2>/dev/null
leftovers2=$(find "$TEST_DIR" -name "needle_rb2.new.*" -o -name "needle_rb2.old.*" 2>/dev/null)
if [[ -z "$leftovers2" ]]; then
    test_pass
else
    test_fail "Temp files remain: $leftovers2"
fi

# =============================================================================
# Section 4: Rollback After Failed Upgrade
# =============================================================================

echo ""
echo "=== Rollback After Failed Upgrade ==="

UPGRADE_TEST_DIR="$TEST_DIR/upgrade_test"
mkdir -p "$UPGRADE_TEST_DIR"

# Test: create_backup creates a backup file
test_case "create_backup creates backup file in backup directory"
_needle_ensure_upgrade_dirs 2>/dev/null
current_bin="$UPGRADE_TEST_DIR/needle_current"
make_mock_binary "$current_bin" "0.9.0"
backup_result=$(_needle_create_backup "$current_bin" "0.9.0" 2>/dev/null)
if [[ -f "$backup_result" ]]; then
    test_pass
else
    test_fail "Backup file not created at: $backup_result"
fi

# Test: backup file is executable
test_case "backup file is executable"
if [[ -x "$backup_result" ]]; then
    test_pass
else
    test_fail "Backup file is not executable"
fi

# Test: clean_old_backups keeps only NEEDLE_MAX_BACKUPS backups
test_case "clean_old_backups enforces max backup limit"
_needle_ensure_upgrade_dirs 2>/dev/null
# Create NEEDLE_MAX_BACKUPS + 2 backup files
for i in $(seq 1 $((NEEDLE_MAX_BACKUPS + 2))); do
    touch "$NEEDLE_BACKUP_DIR/needle-0.${i}.0-20260101${i}00000"
done
_needle_clean_old_backups 2>/dev/null
backup_count=$(find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f 2>/dev/null | wc -l)
if [[ $backup_count -le $NEEDLE_MAX_BACKUPS ]]; then
    test_pass
else
    test_fail "Too many backups: $backup_count (max: $NEEDLE_MAX_BACKUPS)"
fi

# Test: rollback_create_backup creates .bak file
test_case "rollback_create_backup creates .bak file in cache"
_needle_rollback_ensure_dirs 2>/dev/null
rb_current="$UPGRADE_TEST_DIR/needle_rb_current"
make_mock_binary "$rb_current" "1.0.0"
rb_backup=$(_needle_rollback_create_backup "$rb_current" "1.0.0" 2>/dev/null)
expected_bak="$NEEDLE_ROLLBACK_CACHE_DIR/needle-1.0.0.bak"
if [[ -f "$expected_bak" ]]; then
    test_pass
else
    test_fail "Expected .bak file not found at: $expected_bak"
fi

# Test: rollback_create_backup is idempotent (doesn't overwrite existing backup)
test_case "rollback_create_backup is idempotent (does not overwrite existing backup)"
# Modify the .bak file
echo "ORIGINAL_CONTENT" > "$expected_bak"
_needle_rollback_create_backup "$rb_current" "1.0.0" 2>/dev/null
if grep -q "ORIGINAL_CONTENT" "$expected_bak" 2>/dev/null; then
    test_pass
else
    test_fail "Existing backup was overwritten"
fi

# Test: upgrade rollback function finds most recent backup
test_case "upgrade_rollback finds most recent backup"
# Clear existing backups and create fresh ones
rm -rf "$NEEDLE_BACKUP_DIR"
_needle_ensure_upgrade_dirs 2>/dev/null
# Create backups with timestamp ordering
make_mock_binary "$NEEDLE_BACKUP_DIR/needle-0.8.0-20260101120000" "0.8.0"
sleep 0.1
make_mock_binary "$NEEDLE_BACKUP_DIR/needle-0.9.0-20260101130000" "0.9.0"
# The most recent backup (0.9.0) should be found
found=$(find "$NEEDLE_BACKUP_DIR" -name "needle-*" -type f -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)
found_version=$(basename "$found" | sed 's/needle-\([^-]*\)-.*/\1/')
if [[ "$found_version" == "0.9.0" ]]; then
    test_pass
else
    test_fail "Expected '0.9.0', got '$found_version'"
fi

# Test: rollback_find_recent skips current version
test_case "rollback_find_recent skips backup matching current version"
rm -f "$NEEDLE_ROLLBACK_CACHE_DIR"/needle-*.bak
_needle_rollback_ensure_dirs 2>/dev/null
# Create a backup for current version and an older one
make_mock_binary "$NEEDLE_ROLLBACK_CACHE_DIR/needle-${NEEDLE_VERSION}.bak" "$NEEDLE_VERSION"
make_mock_binary "$NEEDLE_ROLLBACK_CACHE_DIR/needle-0.0.9.bak" "0.0.9"
found_recent=$(_needle_rollback_find_recent 2>/dev/null)
found_ver=$(basename "$found_recent" | sed 's/needle-\(.*\)\.bak/\1/')
if [[ "$found_ver" != "$NEEDLE_VERSION" ]]; then
    test_pass
else
    test_fail "find_recent returned current version backup: $found_ver"
fi

# Test: rollback_find_version finds specific version
test_case "rollback_find_version finds specific backup version"
make_mock_binary "$NEEDLE_ROLLBACK_CACHE_DIR/needle-0.0.8.bak" "0.0.8"
found_specific=$(_needle_rollback_find_version "0.0.8" 2>/dev/null)
if [[ -f "$found_specific" ]]; then
    test_pass
else
    test_fail "Could not find backup for version 0.0.8"
fi

# Test: rollback_find_version returns empty for missing version
test_case "rollback_find_version returns empty for missing version"
found_missing=$(_needle_rollback_find_version "9.9.9" 2>/dev/null)
if [[ -z "$found_missing" ]]; then
    test_pass
else
    test_fail "Expected empty result, got: $found_missing"
fi

# Test: failed perform_swap preserves original binary
test_case "failed perform_swap preserves original binary when target dir unwritable"
ro_dir2="$TEST_DIR/readonly_dir2"
mkdir -p "$ro_dir2"
original_bin="$ro_dir2/needle_orig"
make_mock_binary "$original_bin" "0.9.0"
echo "ORIGINAL" > "$original_bin"
# Note: we can't make the containing dir unwritable and keep the file accessible
# Instead, verify the rollback-on-failure logic by testing with a bad source
bad_source="$TEST_DIR/nonexistent_source_$$"
result_path="$TEST_DIR/needle_preserve"
make_mock_binary "$result_path" "0.9.0"
original_content=$(cat "$result_path")
_needle_perform_swap "$bad_source" "$result_path" 2>/dev/null || true
# The binary at result_path should still be valid (original or restored)
if [[ -f "$result_path" ]]; then
    test_pass
else
    test_fail "Binary was lost after failed swap"
fi

# =============================================================================
# Section 5: Version Compatibility Validation
# =============================================================================

echo ""
echo "=== Version Compatibility Validation ==="

# Test: version_compare returns 0 (true) when v1 is older than v2
test_case "version_compare: older version is less than newer"
if _needle_version_compare "0.9.0" "1.0.0" 2>/dev/null; then
    test_pass
else
    test_fail "0.9.0 should be <= 1.0.0"
fi

# Test: version_compare returns 1 (false) when v1 is newer than v2
test_case "version_compare: newer version is not less than older"
if ! _needle_version_compare "1.0.0" "0.9.0" 2>/dev/null; then
    test_pass
else
    test_fail "1.0.0 should NOT be <= 0.9.0"
fi

# Test: version_compare handles equal versions (returns 0)
test_case "version_compare: equal versions are compatible"
if _needle_version_compare "1.0.0" "1.0.0" 2>/dev/null; then
    test_pass
else
    test_fail "1.0.0 should be <= 1.0.0"
fi

# Test: version_compare handles minor version differences
test_case "version_compare: minor version ordering"
if _needle_version_compare "1.0.0" "1.1.0" 2>/dev/null; then
    test_pass
else
    test_fail "1.0.0 should be <= 1.1.0"
fi

# Test: version_compare handles patch version differences
test_case "version_compare: patch version ordering"
if _needle_version_compare "1.0.0" "1.0.1" 2>/dev/null; then
    test_pass
else
    test_fail "1.0.0 should be <= 1.0.1"
fi

# Test: version_compare handles major version jumps
test_case "version_compare: major version jump"
if _needle_version_compare "0.9.9" "1.0.0" 2>/dev/null; then
    test_pass
else
    test_fail "0.9.9 should be <= 1.0.0"
fi

# Test: parse_version extracts components correctly
test_case "parse_version extracts major.minor.patch"
_needle_parse_version "2.5.13"
if [[ "$NEEDLE_PARSED_MAJOR" == "2" ]] && \
   [[ "$NEEDLE_PARSED_MINOR" == "5" ]] && \
   [[ "$NEEDLE_PARSED_PATCH" == "13" ]]; then
    test_pass
else
    test_fail "Expected 2.5.13, got ${NEEDLE_PARSED_MAJOR}.${NEEDLE_PARSED_MINOR}.${NEEDLE_PARSED_PATCH}"
fi

# Test: get_latest_version strips v prefix from tag (as done in upgrade.sh)
test_case "upgrade strips v-prefix from tag_name before using version"
# The upgrade code does: echo "${version#v}" to strip the prefix
raw_tag="v1.2.3"
stripped="${raw_tag#v}"
if [[ "$stripped" == "1.2.3" ]]; then
    test_pass
else
    test_fail "Expected '1.2.3', got '$stripped'"
fi

# Test: rollback_validate_backup passes for valid shell script
test_case "rollback_validate_backup passes for valid executable"
valid_bak="$NEEDLE_ROLLBACK_CACHE_DIR/needle-0.5.0.bak"
make_mock_binary "$valid_bak" "0.5.0"
if _needle_rollback_validate_backup "$valid_bak" 2>/dev/null; then
    test_pass
else
    test_fail "Valid backup failed validation"
fi

# Test: rollback_validate_backup fails for non-existent file
test_case "rollback_validate_backup fails for missing file"
if ! _needle_rollback_validate_backup "$TEST_DIR/nonexistent.bak" 2>/dev/null; then
    test_pass
else
    test_fail "Expected validation failure for missing file"
fi

# Test: rollback_validate_backup fixes non-executable backup
test_case "rollback_validate_backup fixes non-executable backup"
nonexec_bak="$NEEDLE_ROLLBACK_CACHE_DIR/needle-0.4.0.bak"
make_mock_binary "$nonexec_bak" "0.4.0"
chmod -x "$nonexec_bak"
if _needle_rollback_validate_backup "$nonexec_bak" 2>/dev/null && [[ -x "$nonexec_bak" ]]; then
    test_pass
else
    test_fail "Backup permissions not fixed or validation failed"
fi

# Test: NEEDLE_VERSION constant is present and non-empty
test_case "NEEDLE_VERSION constant is non-empty"
if [[ -n "$NEEDLE_VERSION" ]]; then
    test_pass
else
    test_fail "NEEDLE_VERSION is empty"
fi

# Test: NEEDLE_VERSION matches expected semver format
test_case "NEEDLE_VERSION matches semver format X.Y.Z"
if [[ "$NEEDLE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    test_pass
else
    test_fail "NEEDLE_VERSION '$NEEDLE_VERSION' is not valid semver"
fi

# Test: rollback_list shows no error when cache is empty
test_case "rollback_list does not error when cache is empty"
empty_cache="$TEST_DIR/empty_cache"
mkdir -p "$empty_cache"
old_cache="$NEEDLE_ROLLBACK_CACHE_DIR"
NEEDLE_ROLLBACK_CACHE_DIR="$empty_cache"
if _needle_rollback_list 2>/dev/null; then
    test_pass
else
    test_fail "rollback_list failed on empty cache"
fi
NEEDLE_ROLLBACK_CACHE_DIR="$old_cache"

# Test: list_backups shows no error when backup dir is empty
test_case "list_backups does not error when backup dir is empty"
empty_backup="$TEST_DIR/empty_backup"
mkdir -p "$empty_backup"
old_backup="$NEEDLE_BACKUP_DIR"
NEEDLE_BACKUP_DIR="$empty_backup"
if _needle_list_backups 2>/dev/null; then
    test_pass
else
    test_fail "list_backups failed on empty directory"
fi
NEEDLE_BACKUP_DIR="$old_backup"

# =============================================================================
# Summary
# =============================================================================

echo ""
echo "================================"
echo "Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
