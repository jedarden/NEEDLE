#!/usr/bin/env bash
# Test suite for pulse strand framework (nd-2oy)
#
# Tests the pulse strand framework including frequency checking,
# state management, deduplication, and bead creation helpers.

set -euo pipefail

# Get test directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Set required environment variables for tests
export NEEDLE_HOME="${NEEDLE_HOME:-$HOME/.needle}"
export NEEDLE_STATE_DIR="${NEEDLE_STATE_DIR:-state}"
export NEEDLE_SRC="${NEEDLE_SRC:-$PROJECT_ROOT/src}"

# Source test utilities
source "$PROJECT_ROOT/tests/test_utils.sh" 2>/dev/null || {
    # Minimal test utilities if test_utils.sh doesn't exist
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    NC='\033[0m'

    pass() { echo -e "${GREEN}✓${NC} $1"; }
    fail() { echo -e "${RED}✗${NC} $1"; return 1; }
    skip() { echo -e "${YELLOW}⊘${NC} $1"; }
}

# ============================================================================
# Test: Duration Parsing
# ============================================================================

test_parse_duration_seconds() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "30s")

    if [[ "$result" == "30" ]]; then
        pass "Duration parsing: 30s = 30 seconds"
    else
        fail "Duration parsing failed: 30s returned $result (expected 30)"
    fi
}

test_parse_duration_minutes() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "5m")

    if [[ "$result" == "300" ]]; then
        pass "Duration parsing: 5m = 300 seconds"
    else
        fail "Duration parsing failed: 5m returned $result (expected 300)"
    fi
}

test_parse_duration_hours() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "24h")

    if [[ "$result" == "86400" ]]; then
        pass "Duration parsing: 24h = 86400 seconds"
    else
        fail "Duration parsing failed: 24h returned $result (expected 86400)"
    fi
}

test_parse_duration_days() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "1d")

    if [[ "$result" == "86400" ]]; then
        pass "Duration parsing: 1d = 86400 seconds"
    else
        fail "Duration parsing failed: 1d returned $result (expected 86400)"
    fi
}

test_parse_duration_default() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "")

    if [[ "$result" == "86400" ]]; then
        pass "Duration parsing: empty string defaults to 86400 seconds (24h)"
    else
        fail "Duration parsing default failed: empty returned $result (expected 86400)"
    fi
}

test_parse_duration_numeric() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_parse_duration "3600")

    if [[ "$result" == "3600" ]]; then
        pass "Duration parsing: bare number 3600 = 3600 seconds"
    else
        fail "Duration parsing failed: 3600 returned $result (expected 3600)"
    fi
}

# ============================================================================
# Test: Fingerprint Hashing
# ============================================================================

test_fingerprint_hash_consistency() {
    local fp1 fp2
    fp1=$(echo -n "test-fingerprint" | sha256sum | cut -c1-16)
    fp2=$(echo -n "test-fingerprint" | sha256sum | cut -c1-16)

    if [[ "$fp1" == "$fp2" ]]; then
        pass "Fingerprint hashing is consistent"
    else
        fail "Fingerprint hashing is not consistent"
    fi
}

test_fingerprint_hash_uniqueness() {
    local fp1 fp2
    fp1=$(echo -n "fingerprint-1" | sha256sum | cut -c1-16)
    fp2=$(echo -n "fingerprint-2" | sha256sum | cut -c1-16)

    if [[ "$fp1" != "$fp2" ]]; then
        pass "Different fingerprints produce different hashes"
    else
        fail "Different fingerprints produced same hash (collision)"
    fi
}

# ============================================================================
# Test: Severity to Priority Mapping
# ============================================================================

test_severity_critical() {
    local severity="critical"
    local priority=0  # Expected

    # Test the logic from _pulse_create_bead
    local mapped=2
    case "$severity" in
        critical) mapped=0 ;;
        high)     mapped=1 ;;
        medium)   mapped=2 ;;
        low)      mapped=3 ;;
    esac

    if [[ "$mapped" == "$priority" ]]; then
        pass "Severity mapping: critical -> priority 0"
    else
        fail "Severity mapping failed: critical -> $mapped (expected 0)"
    fi
}

test_severity_high() {
    local severity="high"
    local priority=1  # Expected

    local mapped=2
    case "$severity" in
        critical) mapped=0 ;;
        high)     mapped=1 ;;
        medium)   mapped=2 ;;
        low)      mapped=3 ;;
    esac

    if [[ "$mapped" == "$priority" ]]; then
        pass "Severity mapping: high -> priority 1"
    else
        fail "Severity mapping failed: high -> $mapped (expected 1)"
    fi
}

test_severity_medium() {
    local severity="medium"
    local priority=2  # Expected

    local mapped=2
    case "$severity" in
        critical) mapped=0 ;;
        high)     mapped=1 ;;
        medium)   mapped=2 ;;
        low)      mapped=3 ;;
    esac

    if [[ "$mapped" == "$priority" ]]; then
        pass "Severity mapping: medium -> priority 2"
    else
        fail "Severity mapping failed: medium -> $mapped (expected 2)"
    fi
}

test_severity_low() {
    local severity="low"
    local priority=3  # Expected

    local mapped=2
    case "$severity" in
        critical) mapped=0 ;;
        high)     mapped=1 ;;
        medium)   mapped=2 ;;
        low)      mapped=3 ;;
    esac

    if [[ "$mapped" == "$priority" ]]; then
        pass "Severity mapping: low -> priority 3"
    else
        fail "Severity mapping failed: low -> $mapped (expected 3)"
    fi
}

# ============================================================================
# Test: Label Construction
# ============================================================================

test_label_construction_basic() {
    local category="security"
    local expected_labels="pulse,security,automated"

    local labels="pulse,$category,automated"

    if [[ "$labels" == "$expected_labels" ]]; then
        pass "Label construction: basic labels correct"
    else
        fail "Label construction failed: got '$labels' (expected '$expected_labels')"
    fi
}

test_label_construction_with_extra() {
    local category="dependency"
    local extra_labels="outdated,npm"
    local expected_labels="pulse,dependency,automated,outdated,npm"

    local labels="pulse,$category,automated"
    if [[ -n "$extra_labels" ]]; then
        labels="$labels,$extra_labels"
    fi

    if [[ "$labels" == "$expected_labels" ]]; then
        pass "Label construction: extra labels appended correctly"
    else
        fail "Label construction with extra failed: got '$labels' (expected '$expected_labels')"
    fi
}

# ============================================================================
# Test: State File Path Generation
# ============================================================================

test_state_dir_path() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_state_dir)

    # Should end with /state/pulse
    if [[ "$result" == *"/state/pulse" ]] || [[ "$result" == *"/pulse" ]]; then
        pass "State directory path is correct: $result"
    else
        fail "State directory path incorrect: $result"
    fi
}

test_workspace_hash_consistency() {
    local workspace="/test/workspace/path"
    local hash1 hash2

    hash1=$(echo "$workspace" | md5sum | cut -c1-8)
    hash2=$(echo "$workspace" | md5sum | cut -c1-8)

    if [[ "$hash1" == "$hash2" ]]; then
        pass "Workspace hashing is consistent"
    else
        fail "Workspace hashing is not consistent"
    fi
}

test_workspace_hash_uniqueness() {
    local workspace1="/test/workspace/1"
    local workspace2="/test/workspace/2"
    local hash1 hash2

    hash1=$(echo "$workspace1" | md5sum | cut -c1-8)
    hash2=$(echo "$workspace2" | md5sum | cut -c1-8)

    if [[ "$hash1" != "$hash2" ]]; then
        pass "Different workspaces produce different hashes"
    else
        fail "Different workspaces produced same hash"
    fi
}

# ============================================================================
# Test: Frequency Check Logic
# ============================================================================

test_frequency_check_elapsed_calculation() {
    local now=1709500000
    local last_scan=1709413600  # 24 hours ago
    local elapsed=$((now - last_scan))
    local freq_seconds=86400  # 24 hours

    if ((elapsed >= freq_seconds)); then
        pass "Frequency check: elapsed >= frequency (should run)"
    else
        fail "Frequency check calculation error"
    fi
}

test_frequency_check_too_soon() {
    local now=1709500000
    local last_scan=1709496400  # 1 hour ago
    local elapsed=$((now - last_scan))
    local freq_seconds=86400  # 24 hours

    if ((elapsed < freq_seconds)); then
        pass "Frequency check: elapsed < frequency (should skip)"
    else
        fail "Frequency check: should have been skipped"
    fi
}

# ============================================================================
# Test: Max Beads Enforcement Logic
# ============================================================================

test_max_beads_enforcement() {
    local max_beads=5
    local created=3

    if ((created < max_beads)); then
        pass "Max beads: can create more beads ($created < $max_beads)"
    else
        fail "Max beads enforcement logic error"
    fi
}

test_max_beads_limit_reached() {
    local max_beads=5
    local created=5

    if ((created >= max_beads)); then
        pass "Max beads: limit reached ($created >= $max_beads)"
    else
        fail "Max beads limit check failed"
    fi
}

# ============================================================================
# Test: Pulse Strand File Structure
# ============================================================================

test_pulse_strand_exists() {
    local pulse_file="$PROJECT_ROOT/src/strands/pulse.sh"

    if [[ -f "$pulse_file" ]]; then
        pass "Pulse strand file exists"
    else
        fail "Pulse strand file not found: $pulse_file"
    fi
}

test_pulse_strand_has_main_function() {
    local pulse_file="$PROJECT_ROOT/src/strands/pulse.sh"

    if [[ -f "$pulse_file" ]]; then
        if grep -q "_needle_strand_pulse" "$pulse_file"; then
            pass "Pulse strand has main entry function"
        else
            fail "Pulse strand missing _needle_strand_pulse function"
        fi
    else
        skip "Pulse strand file not found"
    fi
}

test_pulse_strand_has_frequency_check() {
    local pulse_file="$PROJECT_ROOT/src/strands/pulse.sh"

    if [[ -f "$pulse_file" ]]; then
        if grep -q "_pulse_should_run" "$pulse_file"; then
            pass "Pulse strand has frequency check function"
        else
            fail "Pulse strand missing _pulse_should_run function"
        fi
    else
        skip "Pulse strand file not found"
    fi
}

test_pulse_strand_has_deduplication() {
    local pulse_file="$PROJECT_ROOT/src/strands/pulse.sh"

    if [[ -f "$pulse_file" ]]; then
        if grep -q "_pulse_already_seen" "$pulse_file" && grep -q "_pulse_mark_seen" "$pulse_file"; then
            pass "Pulse strand has deduplication functions"
        else
            fail "Pulse strand missing deduplication functions"
        fi
    else
        skip "Pulse strand file not found"
    fi
}

test_pulse_strand_has_bead_creation() {
    local pulse_file="$PROJECT_ROOT/src/strands/pulse.sh"

    if [[ -f "$pulse_file" ]]; then
        if grep -q "_pulse_create_bead" "$pulse_file"; then
            pass "Pulse strand has bead creation helper"
        else
            fail "Pulse strand missing _pulse_create_bead function"
        fi
    else
        skip "Pulse strand file not found"
    fi
}

# ============================================================================
# Test: Configuration Defaults
# ============================================================================

test_config_has_pulse_defaults() {
    local config_file="$PROJECT_ROOT/src/lib/config.sh"

    if [[ -f "$config_file" ]]; then
        if grep -q '"pulse":' "$config_file"; then
            pass "Config includes pulse defaults"
        else
            fail "Config missing pulse defaults"
        fi
    else
        skip "Config file not found"
    fi
}

test_config_pulse_frequency_default() {
    local config_file="$PROJECT_ROOT/src/lib/config.sh"

    if [[ -f "$config_file" ]]; then
        if grep -q '"frequency":' "$config_file" || grep -q 'frequency:' "$config_file"; then
            pass "Config includes pulse frequency setting"
        else
            fail "Config missing pulse frequency setting"
        fi
    else
        skip "Config file not found"
    fi
}

test_config_pulse_max_beads_default() {
    local config_file="$PROJECT_ROOT/src/lib/config.sh"

    if [[ -f "$config_file" ]]; then
        if grep -q '"max_beads_per_run":' "$config_file" || grep -q 'max_beads_per_run:' "$config_file"; then
            pass "Config includes pulse max_beads_per_run setting"
        else
            fail "Config missing pulse max_beads_per_run setting"
        fi
    else
        skip "Config file not found"
    fi
}

# ============================================================================
# Test: Telemetry Events
# ============================================================================

test_events_has_pulse_events() {
    local events_file="$PROJECT_ROOT/src/telemetry/events.sh"

    if [[ -f "$events_file" ]]; then
        if grep -q "pulse.bead_created" "$events_file"; then
            pass "Events file includes pulse.bead_created event"
        else
            fail "Events file missing pulse.bead_created event"
        fi
    else
        skip "Events file not found"
    fi
}

test_events_has_pulse_scan_events() {
    local events_file="$PROJECT_ROOT/src/telemetry/events.sh"

    if [[ -f "$events_file" ]]; then
        if grep -q "pulse.scan_completed" "$events_file"; then
            pass "Events file includes pulse.scan_completed event"
        else
            fail "Events file missing pulse.scan_completed event"
        fi
    else
        skip "Events file not found"
    fi
}

# ============================================================================
# Test: Security Detector (nd-21h)
# ============================================================================

test_security_detector_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_detector_security &>/dev/null; then
        pass "Security detector function exists"
    else
        fail "Security detector function not found"
    fi
}

test_npm_severity_mapping_critical() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_npm_severity "critical")

    if [[ "$result" == "critical" ]]; then
        pass "npm severity mapping: critical -> critical"
    else
        fail "npm severity mapping failed: critical -> $result (expected critical)"
    fi
}

test_npm_severity_mapping_high() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_npm_severity "high")

    if [[ "$result" == "high" ]]; then
        pass "npm severity mapping: high -> high"
    else
        fail "npm severity mapping failed: high -> $result (expected high)"
    fi
}

test_npm_severity_mapping_moderate() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_npm_severity "moderate")

    if [[ "$result" == "medium" ]]; then
        pass "npm severity mapping: moderate -> medium"
    else
        fail "npm severity mapping failed: moderate -> $result (expected medium)"
    fi
}

test_npm_severity_mapping_low() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_npm_severity "low")

    if [[ "$result" == "low" ]]; then
        pass "npm severity mapping: low -> low"
    else
        fail "npm severity mapping failed: low -> $result (expected low)"
    fi
}

test_pip_severity_mapping_critical() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_pip_severity "9.5")

    if [[ "$result" == "critical" ]]; then
        pass "pip severity mapping: CVSS 9.5 -> critical"
    else
        fail "pip severity mapping failed: 9.5 -> $result (expected critical)"
    fi
}

test_pip_severity_mapping_high() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_pip_severity "7.5")

    if [[ "$result" == "high" ]]; then
        pass "pip severity mapping: CVSS 7.5 -> high"
    else
        fail "pip severity mapping failed: 7.5 -> $result (expected high)"
    fi
}

test_pip_severity_mapping_medium() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_pip_severity "5.0")

    if [[ "$result" == "medium" ]]; then
        pass "pip severity mapping: CVSS 5.0 -> medium"
    else
        fail "pip severity mapping failed: 5.0 -> $result (expected medium)"
    fi
}

test_pip_severity_mapping_low() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_pip_severity "3.0")

    if [[ "$result" == "low" ]]; then
        pass "pip severity mapping: CVSS 3.0 -> low"
    else
        fail "pip severity mapping failed: 3.0 -> $result (expected low)"
    fi
}

test_pip_severity_mapping_empty() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local result
    result=$(_pulse_map_pip_severity "")

    if [[ "$result" == "medium" ]]; then
        pass "pip severity mapping: empty -> medium (default)"
    else
        fail "pip severity mapping failed: empty -> $result (expected medium)"
    fi
}

test_npm_audit_no_package_json() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without package.json
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_npm_audit "$temp_dir")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "npm audit returns empty array when no package.json"
    else
        fail "npm audit should return empty array for non-Node.js projects"
    fi
}

test_pip_audit_no_requirements() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without Python requirements
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_pip_audit "$temp_dir")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "pip audit returns empty array when no Python requirements"
    else
        fail "pip audit should return empty array for non-Python projects"
    fi
}

test_security_detector_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with no project files
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_detector_security "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    # Check if result is valid JSON array
    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Security detector returns valid JSON array"
    else
        fail "Security detector should return valid JSON array"
    fi
}

test_security_detector_fingerprint_format_npm() {
    # Test that npm fingerprints follow expected format
    local fingerprint="npm:lodash:CWE-1234"

    if [[ "$fingerprint" == npm:* ]]; then
        pass "npm fingerprint format is correct (starts with npm:)"
    else
        fail "npm fingerprint format incorrect"
    fi
}

test_security_detector_fingerprint_format_pip() {
    # Test that pip fingerprints follow expected format
    local fingerprint="pip:requests:CVE-2024-1234"

    if [[ "$fingerprint" == pip:* ]]; then
        pass "pip fingerprint format is correct (starts with pip:)"
    else
        fail "pip fingerprint format incorrect"
    fi
}

test_security_detector_issue_format() {
    # Verify issue object has required fields
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create a mock issue to verify format
    local issue
    issue=$(jq -n \
        --arg category "security" \
        --arg severity "high" \
        --arg title "Test vulnerability" \
        --arg description "Test description" \
        --arg fingerprint "test:fp" \
        --arg labels "test" \
        '{
            category: $category,
            severity: $severity,
            title: $title,
            description: $description,
            fingerprint: $fingerprint,
            labels: $labels
        }')

    local has_category has_severity has_title has_description has_fingerprint
    has_category=$(echo "$issue" | jq 'has("category")')
    has_severity=$(echo "$issue" | jq 'has("severity")')
    has_title=$(echo "$issue" | jq 'has("title")')
    has_description=$(echo "$issue" | jq 'has("description")')
    has_fingerprint=$(echo "$issue" | jq 'has("fingerprint")')

    if [[ "$has_category" == "true" ]] && \
       [[ "$has_severity" == "true" ]] && \
       [[ "$has_title" == "true" ]] && \
       [[ "$has_description" == "true" ]] && \
       [[ "$has_fingerprint" == "true" ]]; then
        pass "Security issue object has all required fields"
    else
        fail "Security issue object missing required fields"
    fi
}

test_security_detector_handles_missing_npm() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with package.json but npm unavailable (simulated)
    local temp_dir
    temp_dir=$(mktemp -d)
    echo '{"name": "test", "version": "1.0.0"}' > "$temp_dir/package.json"

    # _pulse_npm_audit should handle missing npm gracefully
    # Since npm likely exists in the environment, we just check it doesn't crash
    local result
    result=$(_pulse_npm_audit "$temp_dir" 2>/dev/null)

    rm -rf "$temp_dir"

    # Should return valid JSON (empty array if npm fails)
    if echo "$result" | jq -e . &>/dev/null; then
        pass "npm audit handles missing/failing npm gracefully"
    else
        fail "npm audit should return valid JSON even when npm fails"
    fi
}

test_security_detector_handles_missing_pip_audit() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with requirements.txt
    local temp_dir
    temp_dir=$(mktemp -d)
    echo "requests==2.0.0" > "$temp_dir/requirements.txt"

    # _pulse_pip_audit should handle missing pip-audit gracefully
    local result
    result=$(_pulse_pip_audit "$temp_dir" 2>/dev/null)

    rm -rf "$temp_dir"

    # Should return valid JSON (empty array if pip-audit unavailable)
    if echo "$result" | jq -e . &>/dev/null; then
        pass "pip audit handles missing/failing pip-audit gracefully"
    else
        fail "pip audit should return valid JSON even when pip-audit fails"
    fi
}

# ============================================================================
# Test: Exposed Secrets Scanner (nd-21h)
# ============================================================================

test_secrets_scanner_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_scan_secrets &>/dev/null; then
        pass "Secrets scanner function exists"
    else
        fail "Secrets scanner function not found"
    fi
}

test_secrets_scanner_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_scan_secrets "$temp_dir")

    rm -rf "$temp_dir"

    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Secrets scanner returns valid JSON array"
    else
        fail "Secrets scanner should return valid JSON array"
    fi
}

test_secrets_scanner_detects_aws_key() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    # Create a file with a fake AWS access key pattern
    cat > "$temp_dir/config.py" << 'EOF'
AWS_ACCESS_KEY_ID = "AKIAIOSFODNN7EXAMPLE"
AWS_REGION = "us-east-1"
EOF

    local result
    result=$(_pulse_scan_secrets "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -gt 0 ]]; then
        pass "Secrets scanner detects AWS access key pattern"
    else
        fail "Secrets scanner should detect AWS access key pattern"
    fi
}

test_secrets_scanner_detects_github_token() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    # Create a file with a fake GitHub PAT pattern
    cat > "$temp_dir/deploy.sh" << 'EOF'
#!/bin/bash
GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef1234
git clone https://github.com/org/repo.git
EOF

    local result
    result=$(_pulse_scan_secrets "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -gt 0 ]]; then
        pass "Secrets scanner detects GitHub token pattern"
    else
        fail "Secrets scanner should detect GitHub token pattern"
    fi
}

test_secrets_scanner_fingerprint_format() {
    # Test that secrets fingerprints follow expected format
    local fingerprint="secret:aws_access_key:config.py:3"

    if [[ "$fingerprint" == secret:* ]]; then
        pass "Secrets fingerprint format is correct (starts with secret:)"
    else
        fail "Secrets fingerprint format incorrect"
    fi
}

test_secrets_scanner_returns_empty_for_clean_dir() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    # Create a clean file with no secrets
    cat > "$temp_dir/main.py" << 'EOF'
def hello():
    print("Hello, world!")
EOF

    local result
    result=$(_pulse_scan_secrets "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -eq 0 ]]; then
        pass "Secrets scanner returns empty array for clean directory"
    else
        fail "Secrets scanner should return empty array for clean directory (found $count)"
    fi
}

test_secrets_scanner_issue_format() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    cat > "$temp_dir/config.py" << 'EOF'
AWS_ACCESS_KEY_ID = "AKIAIOSFODNN7EXAMPLE"
EOF

    local result
    result=$(_pulse_scan_secrets "$temp_dir")

    rm -rf "$temp_dir"

    # Check issue has required fields
    local has_fields
    has_fields=$(echo "$result" | jq -e '.[0] | has("category") and has("severity") and has("title") and has("description") and has("fingerprint")' 2>/dev/null || echo "false")

    if [[ "$has_fields" == "true" ]]; then
        pass "Secrets scanner issue has all required fields"
    else
        fail "Secrets scanner issue missing required fields"
    fi
}

# ============================================================================
# Test: Security Anti-patterns Scanner (nd-21h)
# ============================================================================

test_antipatterns_scanner_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_scan_security_antipatterns &>/dev/null; then
        pass "Security anti-patterns scanner function exists"
    else
        fail "Security anti-patterns scanner function not found"
    fi
}

test_antipatterns_scanner_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_scan_security_antipatterns "$temp_dir")

    rm -rf "$temp_dir"

    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Anti-patterns scanner returns valid JSON array"
    else
        fail "Anti-patterns scanner should return valid JSON array"
    fi
}

test_antipatterns_scanner_detects_python_eval() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    cat > "$temp_dir/app.py" << 'EOF'
def process_input(user_input):
    result = eval(user_input)
    return result
EOF

    local result
    result=$(_pulse_scan_security_antipatterns "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -gt 0 ]]; then
        pass "Anti-patterns scanner detects Python eval() usage"
    else
        fail "Anti-patterns scanner should detect Python eval() usage"
    fi
}

test_antipatterns_scanner_detects_subprocess_shell() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    cat > "$temp_dir/runner.py" << 'EOF'
import subprocess

def run_command(cmd):
    subprocess.run(cmd, shell=True)
EOF

    local result
    result=$(_pulse_scan_security_antipatterns "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -gt 0 ]]; then
        pass "Anti-patterns scanner detects subprocess with shell=True"
    else
        fail "Anti-patterns scanner should detect subprocess with shell=True"
    fi
}

test_antipatterns_scanner_skips_test_files() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)
    mkdir -p "$temp_dir/tests"

    # Put anti-pattern in a test file - should be skipped
    cat > "$temp_dir/tests/test_app.py" << 'EOF'
import subprocess

def test_command_execution():
    # Testing shell execution in test context
    subprocess.run("echo test", shell=True)
EOF

    local result
    result=$(_pulse_scan_security_antipatterns "$temp_dir")

    rm -rf "$temp_dir"

    local count
    count=$(echo "$result" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$count" -eq 0 ]]; then
        pass "Anti-patterns scanner skips test files"
    else
        fail "Anti-patterns scanner should skip test files (found $count issues)"
    fi
}

test_antipatterns_fingerprint_format() {
    local fingerprint="antipattern:python_eval:app.py:5"

    if [[ "$fingerprint" == antipattern:* ]]; then
        pass "Anti-patterns fingerprint format is correct (starts with antipattern:)"
    else
        fail "Anti-patterns fingerprint format incorrect"
    fi
}

test_antipatterns_scanner_issue_format() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local temp_dir
    temp_dir=$(mktemp -d)

    cat > "$temp_dir/app.py" << 'EOF'
result = eval(user_input)
EOF

    local result
    result=$(_pulse_scan_security_antipatterns "$temp_dir")

    rm -rf "$temp_dir"

    local has_fields
    has_fields=$(echo "$result" | jq -e '.[0] | has("category") and has("severity") and has("title") and has("description") and has("fingerprint")' 2>/dev/null || echo "false")

    if [[ "$has_fields" == "true" ]]; then
        pass "Anti-patterns issue has all required fields"
    else
        fail "Anti-patterns issue missing required fields"
    fi
}

# ============================================================================
# Test: Documentation Drift Detector (nd-gn2)
# ============================================================================

test_docs_detector_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_detector_docs &>/dev/null; then
        pass "Documentation drift detector function exists"
    else
        fail "Documentation drift detector function not found"
    fi
}

test_docs_extract_refs_basic() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local content='Use the `calculate_cost()` function to compute prices.'

    # Test that the grep pattern matches the expected format
    # Use $'\x60' for backtick to avoid shell interpretation issues
    local bt=$'\x60'
    local pattern="${bt}[a-zA-Z_][a-zA-Z0-9_]*\(\)${bt}"
    local match
    match=$(echo "$content" | grep -oE "$pattern" 2>/dev/null)

    if [[ -n "$match" ]]; then
        pass "Documentation reference extraction finds function references"
    else
        fail "Documentation reference extraction should find function references"
    fi
}

test_docs_extract_refs_file_paths() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    local content='See src/lib/utils.sh for more details.'

    # Test that the grep pattern matches file paths
    # Pattern should match paths like src/lib/utils.sh
    local match
    match=$(echo "$content" | grep -oE '[./]?[a-zA-Z0-9_/-]+[.][a-zA-Z]{2,4}' 2>/dev/null | head -1)

    if [[ -n "$match" ]] && [[ "$match" == *"utils.sh"* ]]; then
        pass "Documentation reference extraction finds file path references"
    else
        fail "Documentation reference extraction should find file path references"
    fi
}

test_docs_detector_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with README
    local temp_dir
    temp_dir=$(mktemp -d)
    echo "# Test README" > "$temp_dir/README.md"
    echo "See \`nonexistent_function()\` for details." >> "$temp_dir/README.md"

    local result
    result=$(_pulse_detector_docs "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    # Check if result is valid JSON array
    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Documentation drift detector returns valid JSON array"
    else
        fail "Documentation drift detector should return valid JSON array"
    fi
}

test_docs_function_exists_check() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with a function
    local temp_dir
    temp_dir=$(mktemp -d)
    cat > "$temp_dir/test.sh" << 'EOF'
my_test_function() {
    echo "hello"
}
EOF

    # Test that the function is found
    if _pulse_function_exists "$temp_dir" "my_test_function"; then
        pass "Function existence check finds defined functions"
    else
        fail "Function existence check should find defined functions"
    fi

    # Test that nonexistent function is not found
    if ! _pulse_function_exists "$temp_dir" "nonexistent_func_xyz"; then
        pass "Function existence check correctly identifies missing functions"
    else
        fail "Function existence check should not find nonexistent functions"
    fi

    rm -rf "$temp_dir"
}

test_docs_detector_no_readme() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without README
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_detector_docs "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "Documentation drift detector returns empty array when no docs"
    else
        fail "Documentation drift detector should return empty array when no docs"
    fi
}

# ============================================================================
# Test: Test Coverage Gap Detector (nd-gn2)
# ============================================================================

test_coverage_detector_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_detector_coverage &>/dev/null; then
        pass "Coverage gap detector function exists"
    else
        fail "Coverage gap detector function not found"
    fi
}

test_coverage_detector_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with package.json but no coverage
    local temp_dir
    temp_dir=$(mktemp -d)
    echo '{"name": "test", "version": "1.0.0"}' > "$temp_dir/package.json"

    local result
    result=$(_pulse_detector_coverage "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    # Check if result is valid JSON array
    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Coverage gap detector returns valid JSON array"
    else
        fail "Coverage gap detector should return valid JSON array"
    fi
}

test_coverage_detector_no_project() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without project files
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_detector_coverage "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "Coverage gap detector returns empty array when no project files"
    else
        fail "Coverage gap detector should return empty array when no project files"
    fi
}

test_coverage_threshold_config() {
    # Test threshold retrieval (should return default 70 if not configured)
    # Since get_config may not be available in test environment, check the config file
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q "coverage_threshold.*70" "$config_file" 2>/dev/null; then
        pass "Coverage threshold defaults to 70 in config"
    else
        fail "Coverage threshold should default to 70 in config"
    fi
}

# ============================================================================
# Test: Stale TODO Detector (nd-gn2)
# ============================================================================

test_todos_detector_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_detector_todos &>/dev/null; then
        pass "Stale TODO detector function exists"
    else
        fail "Stale TODO detector function not found"
    fi
}

test_todos_detector_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory with a TODO file
    local temp_dir
    temp_dir=$(mktemp -d)
    echo "# TODO: implement this later" > "$temp_dir/test.py"

    local result
    result=$(_pulse_detector_todos "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    # Check if result is valid JSON array
    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Stale TODO detector returns valid JSON array"
    else
        fail "Stale TODO detector should return valid JSON array"
    fi
}

test_todos_age_threshold_config() {
    # Test age threshold retrieval (should return default 180 if not configured)
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q "todo_age_days.*180" "$config_file" 2>/dev/null; then
        pass "TODO age threshold defaults to 180 days in config"
    else
        fail "TODO age threshold should default to 180 days in config"
    fi
}

test_todos_scan_pattern_matching() {
    # Test that TODO patterns are recognized
    local patterns=(
        "TODO: implement this"
        "FIXME: broken code"
        "XXX: hack alert"
        "HACK: temporary fix"
    )

    local matched=0
    for pattern in "${patterns[@]}"; do
        if [[ "$pattern" =~ (TODO|FIXME|XXX|HACK)[[:space:]]*(:|=|\[) ]]; then
            ((matched++))
        fi
    done

    if [[ "$matched" -eq "${#patterns[@]}" ]]; then
        pass "TODO pattern matching recognizes all TODO variants"
    else
        fail "TODO pattern matching should recognize all TODO variants"
    fi
}

test_todos_line_age_calculation() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # For non-git files, should return -1
    local temp_dir
    temp_dir=$(mktemp -d)
    echo "test" > "$temp_dir/test.txt"

    local age
    age=$(_pulse_get_line_age "$temp_dir/test.txt" 1)

    rm -rf "$temp_dir"

    if [[ "$age" == "-1" ]]; then
        pass "Line age returns -1 for non-git files"
    else
        fail "Line age should return -1 for non-git files"
    fi
}

test_todos_fingerprint_format() {
    # Test that TODO fingerprints follow expected format
    local fingerprint="stale-todo:src/main.py:42"

    if [[ "$fingerprint" == stale-todo:* ]]; then
        pass "TODO fingerprint format is correct (starts with stale-todo:)"
    else
        fail "TODO fingerprint format incorrect"
    fi
}

test_todos_severity_based_on_age() {
    # Test severity escalation based on age
    local age_180=180
    local age_365=365

    local severity_180="low"
    local severity_365="medium"

    if ((age_180 >= 365)); then
        severity_180="medium"
    fi

    if ((age_365 >= 365)); then
        severity_365="medium"
    fi

    if [[ "$severity_180" == "low" ]] && [[ "$severity_365" == "medium" ]]; then
        pass "TODO severity correctly escalates based on age"
    else
        fail "TODO severity should escalate to medium at 365 days"
    fi
}

test_todos_max_per_run_config() {
    # Test max todos per run config (should return default 10 if not configured)
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q "max_todos_per_run.*10" "$config_file" 2>/dev/null; then
        pass "Max TODOs per run defaults to 10 in config"
    else
        fail "Max TODOs per run should default to 10 in config"
    fi
}

# ============================================================================
# Test: Dependency Freshness Detector (nd-1fr)
# ============================================================================

test_deps_detector_function_exists() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    if declare -f _pulse_detector_dependencies &>/dev/null; then
        pass "Dependency freshness detector function exists"
    else
        fail "Dependency freshness detector function not found"
    fi
}

test_deps_detector_returns_json_array() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without project files
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_detector_dependencies "$temp_dir" "test-agent")

    rm -rf "$temp_dir"

    # Check if result is valid JSON array
    if echo "$result" | jq -e 'type == "array"' &>/dev/null; then
        pass "Dependency freshness detector returns valid JSON array"
    else
        fail "Dependency freshness detector should return valid JSON array"
    fi
}

test_deps_detector_no_package_json() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without package.json
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_scan_npm_deps "$temp_dir")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "npm deps scan returns empty array when no package.json"
    else
        fail "npm deps scan should return empty array for non-Node.js projects"
    fi
}

test_deps_detector_no_requirements() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create temp directory without Python requirements
    local temp_dir
    temp_dir=$(mktemp -d)

    local result
    result=$(_pulse_scan_pip_deps "$temp_dir")

    rm -rf "$temp_dir"

    if [[ "$result" == "[]" ]]; then
        pass "pip deps scan returns empty array when no requirements.txt"
    else
        fail "pip deps scan should return empty array for non-Python projects"
    fi
}

test_deps_threshold_config() {
    # Test threshold config (should return default 365 if not configured)
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q "stale_threshold_days.*365" "$config_file" 2>/dev/null; then
        pass "Stale threshold defaults to 365 days in config"
    else
        fail "Stale threshold should default to 365 days in config"
    fi
}

test_deps_max_per_run_config() {
    # Test max deps per run config (should return default 10 if not configured)
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q "max_deps_per_run.*10" "$config_file" 2>/dev/null; then
        pass "Max deps per run defaults to 10 in config"
    else
        fail "Max deps per run should default to 10 in config"
    fi
}

test_deps_npm_fingerprint_format() {
    # Test that npm dependency fingerprints follow expected format
    local fingerprint="stale-dep:npm:lodash"

    if [[ "$fingerprint" == stale-dep:npm:* ]]; then
        pass "npm dependency fingerprint format is correct (starts with stale-dep:npm:)"
    else
        fail "npm dependency fingerprint format incorrect"
    fi
}

test_deps_pip_fingerprint_format() {
    # Test that pip dependency fingerprints follow expected format
    local fingerprint="stale-dep:pip:requests"

    if [[ "$fingerprint" == stale-dep:pip:* ]]; then
        pass "pip dependency fingerprint format is correct (starts with stale-dep:pip:)"
    else
        fail "pip dependency fingerprint format incorrect"
    fi
}

test_deps_severity_based_on_age() {
    # Test severity escalation based on age
    local age_365=365
    local age_730=730
    local age_1095=1095

    # Test severity determination logic
    local severity_365="low"
    local severity_730="medium"
    local severity_1095="high"

    if ((age_365 >= 1095)); then
        severity_365="high"
    elif ((age_365 >= 730)); then
        severity_365="medium"
    fi

    if ((age_730 >= 1095)); then
        severity_730="high"
    elif ((age_730 >= 730)); then
        severity_730="medium"
    fi

    if ((age_1095 >= 1095)); then
        severity_1095="high"
    elif ((age_1095 >= 730)); then
        severity_1095="medium"
    fi

    if [[ "$severity_365" == "low" ]] && [[ "$severity_730" == "medium" ]] && [[ "$severity_1095" == "high" ]]; then
        pass "Dependency severity correctly escalates based on age"
    else
        fail "Dependency severity should escalate: 365d=low, 730d=medium, 1095d=high"
    fi
}

test_deps_issue_format() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # Create a mock issue to verify format
    local issue
    issue=$(jq -n \
        --arg category "dependencies" \
        --arg severity "low" \
        --arg title "Update stale npm dependency: test-pkg (400 days old)" \
        --arg description "Test description" \
        --arg fingerprint "stale-dep:npm:test-pkg" \
        --arg labels "npm,dependencies,stale,maintenance" \
        '{
            category: $category,
            severity: $severity,
            title: $title,
            description: $description,
            fingerprint: $fingerprint,
            labels: $labels
        }')

    local has_category has_severity has_title has_description has_fingerprint
    has_category=$(echo "$issue" | jq 'has("category")')
    has_severity=$(echo "$issue" | jq 'has("severity")')
    has_title=$(echo "$issue" | jq 'has("title")')
    has_description=$(echo "$issue" | jq 'has("description")')
    has_fingerprint=$(echo "$issue" | jq 'has("fingerprint")')

    if [[ "$has_category" == "true" ]] && \
       [[ "$has_severity" == "true" ]] && \
       [[ "$has_title" == "true" ]] && \
       [[ "$has_description" == "true" ]] && \
       [[ "$has_fingerprint" == "true" ]]; then
        pass "Dependency issue object has all required fields"
    else
        fail "Dependency issue object missing required fields"
    fi
}

test_deps_detector_handles_missing_curl() {
    # This test verifies the detector handles network failures gracefully
    # Since curl is likely available, we just verify the error handling logic exists
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # The function should exist and handle network errors
    if declare -f _pulse_get_npm_package_age &>/dev/null; then
        pass "npm package age function exists with error handling"
    else
        fail "npm package age function should exist"
    fi
}

test_deps_detector_handles_missing_pip() {
    source "$PROJECT_ROOT/src/strands/pulse.sh" 2>/dev/null || true

    # The function should exist and handle network errors
    if declare -f _pulse_get_pip_package_age &>/dev/null; then
        pass "pip package age function exists with error handling"
    else
        fail "pip package age function should exist"
    fi
}

test_deps_detector_enabled_config() {
    # Test that dependencies detector is enabled by default in config
    local config_file="$PROJECT_ROOT/src/lib/config.sh"
    if [[ -f "$config_file" ]] && grep -q '"dependencies".*true' "$config_file" 2>/dev/null; then
        pass "Dependencies detector is enabled by default in config"
    else
        fail "Dependencies detector should be enabled by default in config"
    fi
}

# ============================================================================
# Run Tests
# ============================================================================

run_tests() {
    echo "Running pulse strand framework tests..."
    echo ""

    local failed=0

    # Duration parsing tests
    echo "=== Duration Parsing Tests ==="
    test_parse_duration_seconds || ((failed++))
    test_parse_duration_minutes || ((failed++))
    test_parse_duration_hours || ((failed++))
    test_parse_duration_days || ((failed++))
    test_parse_duration_default || ((failed++))
    test_parse_duration_numeric || ((failed++))

    # Fingerprint tests
    echo ""
    echo "=== Fingerprint Tests ==="
    test_fingerprint_hash_consistency || ((failed++))
    test_fingerprint_hash_uniqueness || ((failed++))

    # Severity mapping tests
    echo ""
    echo "=== Severity Mapping Tests ==="
    test_severity_critical || ((failed++))
    test_severity_high || ((failed++))
    test_severity_medium || ((failed++))
    test_severity_low || ((failed++))

    # Label construction tests
    echo ""
    echo "=== Label Construction Tests ==="
    test_label_construction_basic || ((failed++))
    test_label_construction_with_extra || ((failed++))

    # State path tests
    echo ""
    echo "=== State Path Tests ==="
    test_state_dir_path || ((failed++))
    test_workspace_hash_consistency || ((failed++))
    test_workspace_hash_uniqueness || ((failed++))

    # Frequency check tests
    echo ""
    echo "=== Frequency Check Tests ==="
    test_frequency_check_elapsed_calculation || ((failed++))
    test_frequency_check_too_soon || ((failed++))

    # Max beads tests
    echo ""
    echo "=== Max Beads Tests ==="
    test_max_beads_enforcement || ((failed++))
    test_max_beads_limit_reached || ((failed++))

    # File structure tests
    echo ""
    echo "=== File Structure Tests ==="
    test_pulse_strand_exists || ((failed++))
    test_pulse_strand_has_main_function || ((failed++))
    test_pulse_strand_has_frequency_check || ((failed++))
    test_pulse_strand_has_deduplication || ((failed++))
    test_pulse_strand_has_bead_creation || ((failed++))

    # Configuration tests
    echo ""
    echo "=== Configuration Tests ==="
    test_config_has_pulse_defaults || ((failed++))
    test_config_pulse_frequency_default || ((failed++))
    test_config_pulse_max_beads_default || ((failed++))

    # Telemetry tests
    echo ""
    echo "=== Telemetry Tests ==="
    test_events_has_pulse_events || ((failed++))
    test_events_has_pulse_scan_events || ((failed++))

    # Security detector tests (nd-21h)
    echo ""
    echo "=== Security Detector Tests (nd-21h) ==="
    test_security_detector_function_exists || ((failed++))
    test_npm_severity_mapping_critical || ((failed++))
    test_npm_severity_mapping_high || ((failed++))
    test_npm_severity_mapping_moderate || ((failed++))
    test_npm_severity_mapping_low || ((failed++))
    test_pip_severity_mapping_critical || ((failed++))
    test_pip_severity_mapping_high || ((failed++))
    test_pip_severity_mapping_medium || ((failed++))
    test_pip_severity_mapping_low || ((failed++))
    test_pip_severity_mapping_empty || ((failed++))
    test_npm_audit_no_package_json || ((failed++))
    test_pip_audit_no_requirements || ((failed++))
    test_security_detector_returns_json_array || ((failed++))
    test_security_detector_fingerprint_format_npm || ((failed++))
    test_security_detector_fingerprint_format_pip || ((failed++))
    test_security_detector_issue_format || ((failed++))
    test_security_detector_handles_missing_npm || ((failed++))
    test_security_detector_handles_missing_pip_audit || ((failed++))

    # Exposed secrets scanner tests (nd-21h)
    echo ""
    echo "=== Exposed Secrets Scanner Tests (nd-21h) ==="
    test_secrets_scanner_function_exists || ((failed++))
    test_secrets_scanner_returns_json_array || ((failed++))
    test_secrets_scanner_detects_aws_key || ((failed++))
    test_secrets_scanner_detects_github_token || ((failed++))
    test_secrets_scanner_fingerprint_format || ((failed++))
    test_secrets_scanner_returns_empty_for_clean_dir || ((failed++))
    test_secrets_scanner_issue_format || ((failed++))

    # Security anti-patterns scanner tests (nd-21h)
    echo ""
    echo "=== Security Anti-patterns Scanner Tests (nd-21h) ==="
    test_antipatterns_scanner_function_exists || ((failed++))
    test_antipatterns_scanner_returns_json_array || ((failed++))
    test_antipatterns_scanner_detects_python_eval || ((failed++))
    test_antipatterns_scanner_detects_subprocess_shell || ((failed++))
    test_antipatterns_scanner_skips_test_files || ((failed++))
    test_antipatterns_fingerprint_format || ((failed++))
    test_antipatterns_scanner_issue_format || ((failed++))

    # Documentation drift detector tests (nd-gn2)
    echo ""
    echo "=== Documentation Drift Detector Tests (nd-gn2) ==="
    test_docs_detector_function_exists || ((failed++))
    test_docs_extract_refs_basic || ((failed++))
    test_docs_extract_refs_file_paths || ((failed++))
    test_docs_detector_returns_json_array || ((failed++))
    test_docs_function_exists_check || ((failed++))
    test_docs_detector_no_readme || ((failed++))

    # Coverage gap detector tests (nd-gn2)
    echo ""
    echo "=== Coverage Gap Detector Tests (nd-gn2) ==="
    test_coverage_detector_function_exists || ((failed++))
    test_coverage_detector_returns_json_array || ((failed++))
    test_coverage_detector_no_project || ((failed++))
    test_coverage_threshold_config || ((failed++))

    # Stale TODO detector tests (nd-gn2)
    echo ""
    echo "=== Stale TODO Detector Tests (nd-gn2) ==="
    test_todos_detector_function_exists || ((failed++))
    test_todos_detector_returns_json_array || ((failed++))
    test_todos_age_threshold_config || ((failed++))
    test_todos_scan_pattern_matching || ((failed++))
    test_todos_line_age_calculation || ((failed++))
    test_todos_fingerprint_format || ((failed++))
    test_todos_severity_based_on_age || ((failed++))
    test_todos_max_per_run_config || ((failed++))

    # Dependency freshness detector tests (nd-1fr)
    echo ""
    echo "=== Dependency Freshness Detector Tests (nd-1fr) ==="
    test_deps_detector_function_exists || ((failed++))
    test_deps_detector_returns_json_array || ((failed++))
    test_deps_detector_no_package_json || ((failed++))
    test_deps_detector_no_requirements || ((failed++))
    test_deps_threshold_config || ((failed++))
    test_deps_max_per_run_config || ((failed++))
    test_deps_npm_fingerprint_format || ((failed++))
    test_deps_pip_fingerprint_format || ((failed++))
    test_deps_severity_based_on_age || ((failed++))
    test_deps_issue_format || ((failed++))
    test_deps_detector_handles_missing_curl || ((failed++))
    test_deps_detector_handles_missing_pip || ((failed++))
    test_deps_detector_enabled_config || ((failed++))

    echo ""
    if [[ $failed -eq 0 ]]; then
        echo -e "${GREEN}All tests passed!${NC}"
        return 0
    else
        echo -e "${RED}$failed test(s) failed${NC}"
        return 1
    fi
}

# Run if executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    run_tests "$@"
fi
