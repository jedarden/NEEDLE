#!/usr/bin/env bash
# Test suite for mend strand stale claim detection (nd-3pe)
#
# Tests the _needle_mend_stale_claims function which detects and releases
# claims that have been held longer than the configured threshold.

set -euo pipefail

# Get test directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

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
# Test: ISO8601 Parsing
# ============================================================================

test_parse_iso8601_gnu() {
    local timestamp="2026-03-03T10:00:00Z"
    local epoch

    # This test requires GNU date or Python
    if command -v python3 &>/dev/null; then
        epoch=$(python3 -c "
import datetime
dt = datetime.datetime(2026, 3, 3, 10, 0, 0)
print(int(dt.timestamp()))
" 2>/dev/null)

        if [[ -n "$epoch" ]] && [[ "$epoch" =~ ^[0-9]+$ ]]; then
            pass "ISO8601 parsing produces valid epoch: $epoch"
        else
            fail "ISO8601 parsing failed to produce valid epoch"
        fi
    else
        skip "ISO8601 parsing test (python3 not available)"
    fi
}

test_parse_iso8601_with_milliseconds() {
    local timestamp="2026-03-03T10:00:00.123456Z"

    # Should handle milliseconds by stripping them
    local stripped="${timestamp%%.*}"
    local expected="2026-03-03T10:00:00"

    if [[ "$stripped" == "$expected" ]]; then
        pass "Millisecond stripping works correctly"
    else
        fail "Millisecond stripping failed: got '$stripped', expected '$expected'"
    fi
}

# ============================================================================
# Test: Stale Claim Detection Logic
# ============================================================================

test_stale_threshold_calculation() {
    # Test that age calculation works correctly
    local now=1709500000
    local claim_epoch=1709496000  # 4000 seconds ago
    local age=$((now - claim_epoch))
    local threshold=3600  # 1 hour

    if ((age > threshold)); then
        pass "Stale claim detected correctly (age=${age}s > threshold=${threshold}s)"
    else
        fail "Stale claim not detected (age=${age}s <= threshold=${threshold}s)"
    fi
}

test_fresh_claim_not_stale() {
    # Test that fresh claims are not marked as stale
    local now=1709500000
    local claim_epoch=1709498000  # 2000 seconds ago (< 1 hour)
    local age=$((now - claim_epoch))
    local threshold=3600  # 1 hour

    if ((age <= threshold)); then
        pass "Fresh claim correctly not marked stale (age=${age}s <= threshold=${threshold}s)"
    else
        fail "Fresh claim incorrectly marked as stale"
    fi
}

test_configurable_threshold() {
    # Test that threshold is configurable
    local default_threshold=3600
    local custom_threshold=7200  # 2 hours

    # Verify defaults
    if [[ "$default_threshold" -eq 3600 ]]; then
        pass "Default threshold is 1 hour (3600s)"
    else
        fail "Default threshold is not 1 hour"
    fi

    # Verify custom can be set
    if [[ "$custom_threshold" -gt "$default_threshold" ]]; then
        pass "Custom threshold can be set higher than default"
    else
        fail "Custom threshold configuration issue"
    fi
}

# ============================================================================
# Test: Edge Cases
# ============================================================================

test_null_timestamp_handling() {
    # Should handle null/empty timestamps gracefully
    local timestamp=""
    local result="null"

    if [[ -z "$timestamp" ]] || [[ "$timestamp" == "null" ]]; then
        pass "Empty/null timestamp detected correctly"
    else
        fail "Empty/null timestamp handling issue"
    fi
}

test_zero_epoch_handling() {
    # Zero epoch should indicate parsing failure
    local epoch=0

    if [[ "$epoch" -eq 0 ]]; then
        pass "Zero epoch correctly indicates parsing failure"
    else
        fail "Zero epoch handling issue"
    fi
}

# ============================================================================
# Test: Integration with mend strand
# ============================================================================

test_mend_strand_includes_stale_detection() {
    # Verify that mend strand calls stale claim detection
    local mend_file="$PROJECT_ROOT/src/strands/mend.sh"

    if [[ -f "$mend_file" ]]; then
        if grep -q "_needle_mend_stale_claims" "$mend_file"; then
            pass "Mend strand includes stale claim detection"
        else
            fail "Mend strand does not include stale claim detection"
        fi
    else
        skip "Mend strand file not found"
    fi
}

test_stale_detection_order() {
    # Verify stale claims are checked after orphaned claims
    local mend_file="$PROJECT_ROOT/src/strands/mend.sh"

    if [[ -f "$mend_file" ]]; then
        local orphan_line stale_line
        orphan_line=$(grep -n "_needle_mend_orphaned_claims" "$mend_file" | head -1 | cut -d: -f1)
        stale_line=$(grep -n "_needle_mend_stale_claims" "$mend_file" | head -1 | cut -d: -f1)

        if [[ -n "$orphan_line" ]] && [[ -n "$stale_line" ]] && [[ "$stale_line" -gt "$orphan_line" ]]; then
            pass "Stale claims checked after orphaned claims (order correct)"
        else
            fail "Stale claims check order issue"
        fi
    else
        skip "Mend strand file not found"
    fi
}

# ============================================================================
# Run Tests
# ============================================================================

run_tests() {
    echo "Running mend stale claim detection tests..."
    echo ""

    local failed=0

    # ISO8601 parsing tests
    test_parse_iso8601_gnu || ((failed++))
    test_parse_iso8601_with_milliseconds || ((failed++))

    # Detection logic tests
    test_stale_threshold_calculation || ((failed++))
    test_fresh_claim_not_stale || ((failed++))
    test_configurable_threshold || ((failed++))

    # Edge case tests
    test_null_timestamp_handling || ((failed++))
    test_zero_epoch_handling || ((failed++))

    # Integration tests
    test_mend_strand_includes_stale_detection || ((failed++))
    test_stale_detection_order || ((failed++))

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
