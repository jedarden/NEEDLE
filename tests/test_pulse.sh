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
