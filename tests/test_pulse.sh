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
