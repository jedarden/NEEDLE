#!/usr/bin/env bash
# Tests for src/lib/config_schema.sh
#
# Covers:
#   - Valid config passes schema validation
#   - Invalid field types are rejected with clear messages
#   - Out-of-range numeric values are rejected
#   - Invalid enum values are rejected
#   - Deprecated keys emit warnings but do not fail
#   - Unknown strand names under strands: are rejected
#   - Invalid strand flag values (not true/false/auto) are rejected
#   - Missing/empty config file is handled gracefully
#   - Invalid YAML syntax is rejected early

# Don't use set -e - arithmetic can return 1
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Set up isolated test environment
NEEDLE_HOME="$(mktemp -d)"
NEEDLE_CONFIG_NAME="config.yaml"
NEEDLE_CONFIG_FILE="$NEEDLE_HOME/$NEEDLE_CONFIG_NAME"

export NEEDLE_HOME NEEDLE_CONFIG_NAME NEEDLE_CONFIG_FILE
export NEEDLE_QUIET=true

# Source dependencies
source "$PROJECT_ROOT/src/lib/constants.sh"
source "$PROJECT_ROOT/src/lib/output.sh"
source "$PROJECT_ROOT/src/lib/utils.sh"
source "$PROJECT_ROOT/src/lib/config.sh"
source "$PROJECT_ROOT/src/lib/config_schema.sh"

# Cleanup on exit
cleanup() { rm -rf "$NEEDLE_HOME"; }
trap cleanup EXIT

# ============================================================================
# Test framework
# ============================================================================
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

_t() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "TEST: $name ... "
}

_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

_fail() {
    echo "FAIL"
    [[ -n "$1" ]] && echo "  => $1"
    ((TESTS_FAILED++))
}

# Write a config file with given content
_write_config() {
    cat > "$NEEDLE_CONFIG_FILE"
}

# Assert a command/function returns 0 (success)
# Usage: _assert_ok "test name" command [args...]
_assert_ok() {
    local name="$1"; shift
    _t "$name"
    if "$@" 2>/dev/null; then
        _pass
    else
        _fail "Expected success, got failure"
    fi
}

# Assert a command/function returns non-zero (failure)
# Usage: _assert_fail "test name" command [args...]
_assert_fail() {
    local name="$1"; shift
    _t "$name"
    if ! "$@" 2>/dev/null; then
        _pass
    else
        _fail "Expected failure, got success"
    fi
}

# ============================================================================
# Tests: validate_config_schema
# ============================================================================

echo ""
echo "========================================"
echo "validate_config_schema tests"
echo "========================================"

# Non-existent config is fine
rm -f "$NEEDLE_CONFIG_FILE"
_assert_ok "non-existent config file returns success" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# Empty config is fine
touch "$NEEDLE_CONFIG_FILE"
_assert_ok "empty config file returns success" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# Valid minimal config
_write_config <<'EOF'
billing:
  model: pay_per_token
  daily_budget_usd: 10.0
limits:
  global_max_concurrent: 5
EOF
_assert_ok "valid minimal config passes" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# Valid full config
_write_config <<'EOF'
billing:
  model: use_or_lose
  daily_budget_usd: 25.0
limits:
  global_max_concurrent: 20
  providers:
    anthropic:
      max_concurrent: 5
      requests_per_minute: 60
runner:
  polling_interval: 2s
  idle_timeout: 300s
strands:
  - src/strands/pluck.sh
  - src/strands/explore.sh
  - src/strands/mend.sh
mend:
  heartbeat_max_age: 3600
  max_log_files: 100
  min_interval: 60
hooks:
  timeout: 30s
  fail_action: warn
mitosis:
  enabled: true
  max_children: 5
  min_children: 2
  min_complexity: 3
  timeout: 60
watchdog:
  recovery_action: restart
pulse:
  max_beads_per_run: 5
  coverage_threshold: 70
  detectors:
    security: true
    dependencies: true
    docs: true
    coverage: false
    todos: true
select:
  work_stealing_enabled: true
  work_stealing_timeout: 1800
  check_worker_heartbeat: true
  unassigned_by_default: true
  stealing_load_threshold: 2
updates:
  auto_upgrade: false
  disabled: false
file_locks:
  stale_action: warn
fabric:
  enabled: false
  timeout: 2
  batching: false
EOF
_assert_ok "valid full config passes" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Invalid enum values
# ============================================================================

echo ""
echo "========================================"
echo "Invalid enum value tests"
echo "========================================"

_write_config <<'EOF'
billing:
  model: freemium
EOF
_assert_fail "billing.model=freemium is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
hooks:
  fail_action: crash
EOF
_assert_fail "hooks.fail_action=crash is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
watchdog:
  recovery_action: kill
EOF
_assert_fail "watchdog.recovery_action=kill is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  stale_action: delete
EOF
_assert_fail "file_locks.stale_action=delete is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Invalid boolean values
# ============================================================================

echo ""
echo "========================================"
echo "Invalid boolean value tests"
echo "========================================"

_write_config <<'EOF'
mitosis:
  enabled: yes_please
EOF
_assert_fail "mitosis.enabled=yes_please is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
select:
  work_stealing_enabled: maybe
EOF
_assert_fail "select.work_stealing_enabled=maybe is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
updates:
  auto_upgrade: enabled
EOF
_assert_fail "updates.auto_upgrade=enabled is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  ld_preload: enabled
EOF
_assert_fail "file_locks.ld_preload=enabled is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Invalid integer values
# ============================================================================

echo ""
echo "========================================"
echo "Invalid integer value tests"
echo "========================================"

_write_config <<'EOF'
limits:
  global_max_concurrent: many
EOF
_assert_fail "limits.global_max_concurrent=many is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
limits:
  global_max_concurrent: 0
EOF
_assert_fail "limits.global_max_concurrent=0 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  max_children: -1
EOF
_assert_fail "mitosis.max_children=-1 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  max_children: 3.5
EOF
_assert_fail "mitosis.max_children=3.5 is rejected (must be integer)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
pulse:
  coverage_threshold: 150
EOF
_assert_fail "pulse.coverage_threshold=150 is rejected (max=100)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
pulse:
  coverage_threshold: -5
EOF
_assert_fail "pulse.coverage_threshold=-5 is rejected (min=0)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Invalid float values
# ============================================================================

echo ""
echo "========================================"
echo "Invalid float value tests"
echo "========================================"

_write_config <<'EOF'
billing:
  daily_budget_usd: lots
EOF
_assert_fail "billing.daily_budget_usd=lots is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Invalid duration values
# ============================================================================

echo ""
echo "========================================"
echo "Invalid duration value tests"
echo "========================================"

_write_config <<'EOF'
runner:
  polling_interval: two-seconds
EOF
_assert_fail "runner.polling_interval=two-seconds is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
hooks:
  timeout: forever
EOF
_assert_fail "hooks.timeout=forever is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
runner:
  polling_interval: 30s
  idle_timeout: 5m
EOF
_assert_ok "valid durations with units (30s, 5m) pass" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
runner:
  polling_interval: 30
EOF
_assert_ok "bare number duration passes" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: validate_strand_config
# ============================================================================

echo ""
echo "========================================"
echo "Strand config validation tests"
echo "========================================"

_write_config <<'EOF'
strands:
  - src/strands/pluck.sh
  - src/strands/explore.sh
  - src/strands/mend.sh
EOF
_assert_ok "strands as list of script paths passes" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  pluck: true
  fetch: auto
EOF
_assert_fail "unknown strand 'fetch' is rejected" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  pluck: true
  harvest: true
  weave: false
EOF
_assert_fail "unknown strand 'harvest' is rejected" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  pluck: enabled
EOF
_assert_fail "strand flag value 'enabled' is rejected" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  weave: yes
EOF
_assert_fail "strand flag value 'yes' is rejected" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
billing:
  model: pay_per_token
EOF
_assert_ok "config without strands section passes strand validation" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

# Error message quality: map format rejected with helpful message
_t "strand map format rejected with 'expected a list' message"
_write_config <<'EOF'
strands:
  bogus: true
EOF
err_msg=$(validate_strand_config "$NEEDLE_CONFIG_FILE" 2>&1)
if echo "$err_msg" | grep -q "expected a list"; then
    _pass
else
    _fail "Error should say 'expected a list'. Got: $err_msg"
fi

# Error message quality: any map rejected even with known strand names
_t "strand map with valid name rejected with 'expected a list' message"
_write_config <<'EOF'
strands:
  pluck: true
EOF
err_msg=$(validate_strand_config "$NEEDLE_CONFIG_FILE" 2>&1)
if echo "$err_msg" | grep -q "expected a list"; then
    _pass
else
    _fail "Error should say 'expected a list'. Got: $err_msg"
fi

# ============================================================================
# Tests: check_deprecated_keys
# ============================================================================

echo ""
echo "========================================"
echo "Deprecated key detection tests"
echo "========================================"

# check_deprecated_keys always returns 0 (warnings, not errors)
_write_config <<'EOF'
effort:
  budget:
    daily_limit_usd: 50.0
EOF
_assert_ok "check_deprecated_keys always returns 0 (never blocks)" \
    check_deprecated_keys "$NEEDLE_CONFIG_FILE"

# Deprecated key emits a warning message
_t "deprecated effort.budget.daily_limit_usd emits a warning"
_write_config <<'EOF'
effort:
  budget:
    daily_limit_usd: 50.0
EOF
warn_out=$(check_deprecated_keys "$NEEDLE_CONFIG_FILE" 2>&1)
if echo "$warn_out" | grep -qi "deprecated\|billing.daily_budget_usd"; then
    _pass
else
    _fail "Expected deprecation warning. Got: $warn_out"
fi

# Clean config emits no warnings
_t "clean config emits no deprecation warnings"
_write_config <<'EOF'
billing:
  model: pay_per_token
  daily_budget_usd: 10.0
EOF
clean_warn=$(check_deprecated_keys "$NEEDLE_CONFIG_FILE" 2>&1)
if [[ -z "$clean_warn" ]]; then
    _pass
else
    _fail "No warnings expected for clean config. Got: $clean_warn"
fi

# Deprecated runner.max_workers warns
_t "deprecated runner.max_workers emits a warning"
_write_config <<'EOF'
runner:
  max_workers: 10
EOF
runner_warn=$(check_deprecated_keys "$NEEDLE_CONFIG_FILE" 2>&1)
if echo "$runner_warn" | grep -qi "deprecated\|max_workers"; then
    _pass
else
    _fail "Expected deprecation warning for runner.max_workers. Got: $runner_warn"
fi

# ============================================================================
# Tests: validate_config_on_load
# ============================================================================

echo ""
echo "========================================"
echo "validate_config_on_load tests"
echo "========================================"

_write_config <<'EOF'
billing:
  model: pay_per_token
strands:
  - src/strands/pluck.sh
  - src/strands/explore.sh
EOF
_assert_ok "validate_config_on_load returns 0 for valid config" \
    validate_config_on_load "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
billing:
  model: invalid_model
EOF
_assert_fail "validate_config_on_load returns 1 for invalid config" \
    validate_config_on_load "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: validate_config integration with schema module
# ============================================================================

echo ""
echo "========================================"
echo "validate_config integration tests"
echo "========================================"

_write_config <<'EOF'
billing:
  model: pay_per_token
  daily_budget_usd: 10.0
limits:
  global_max_concurrent: 5
mitosis:
  enabled: true
  max_children: 5
  min_complexity: 3
EOF
_assert_ok "validate_config passes with valid config (uses schema)" \
    validate_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  pluck: true
  nonexistent_strand: auto
EOF
_assert_fail "validate_config fails for strands in old map format" \
    validate_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
billing:
  model: wrong_model
EOF
_assert_fail "validate_config fails for invalid billing model" \
    validate_config "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: YAML syntax errors
# ============================================================================

echo ""
echo "========================================"
echo "YAML syntax error tests"
echo "========================================"

_write_config <<'EOF'
billing:
  model: pay_per_token
  broken: [unclosed
EOF
_assert_fail "invalid YAML syntax (unclosed bracket) is rejected" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Valid enum/value edge cases
# ============================================================================

echo ""
echo "========================================"
echo "Valid value edge cases"
echo "========================================"

_write_config <<'EOF'
billing:
  model: pay_per_token
EOF
_assert_ok "billing.model=pay_per_token is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
billing:
  model: use_or_lose
EOF
_assert_ok "billing.model=use_or_lose is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
billing:
  model: unlimited
EOF
_assert_ok "billing.model=unlimited is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
strands:
  - src/strands/pluck.sh
  - src/strands/explore.sh
  - src/strands/mend.sh
EOF
_assert_ok "strands list with multiple entries is valid" \
    validate_strand_config "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
watchdog:
  recovery_action: stop
EOF
_assert_ok "watchdog.recovery_action=stop is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  stale_action: release
EOF
_assert_ok "file_locks.stale_action=release is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  stale_action: ignore
EOF
_assert_ok "file_locks.stale_action=ignore is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  ld_preload: true
EOF
_assert_ok "file_locks.ld_preload=true is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  ld_preload: false
EOF
_assert_ok "file_locks.ld_preload=false is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
file_locks:
  ld_preload: true
  ld_preload_lib: /usr/local/lib/libcheckout.so
EOF
_assert_ok "file_locks.ld_preload with ld_preload_lib path is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
hooks:
  fail_action: abort
EOF
_assert_ok "hooks.fail_action=abort is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
hooks:
  fail_action: ignore
EOF
_assert_ok "hooks.fail_action=ignore is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# pulse.coverage_threshold=0 is valid (min=0)
_write_config <<'EOF'
pulse:
  coverage_threshold: 0
EOF
_assert_ok "pulse.coverage_threshold=0 is valid (boundary)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# pulse.coverage_threshold=100 is valid (max=100)
_write_config <<'EOF'
pulse:
  coverage_threshold: 100
EOF
_assert_ok "pulse.coverage_threshold=100 is valid (boundary)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# mitosis.min_complexity=0 is valid (min=0)
_write_config <<'EOF'
mitosis:
  min_complexity: 0
EOF
_assert_ok "mitosis.min_complexity=0 is valid (min boundary)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Force mitosis config validation
# ============================================================================

echo ""
echo "========================================"
echo "Force mitosis config validation tests"
echo "========================================"

# mitosis.force_on_failure accepts valid booleans
_write_config <<'EOF'
mitosis:
  force_on_failure: true
EOF
_assert_ok "mitosis.force_on_failure=true is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_on_failure: false
EOF
_assert_ok "mitosis.force_on_failure=false is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_on_failure: maybe
EOF
_assert_fail "mitosis.force_on_failure=maybe is rejected (not boolean)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# mitosis.force_failure_threshold accepts valid integers >= 1
_write_config <<'EOF'
mitosis:
  force_failure_threshold: 3
EOF
_assert_ok "mitosis.force_failure_threshold=3 is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_failure_threshold: 1
EOF
_assert_ok "mitosis.force_failure_threshold=1 is valid (min boundary)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_failure_threshold: 0
EOF
_assert_fail "mitosis.force_failure_threshold=0 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_failure_threshold: -1
EOF
_assert_fail "mitosis.force_failure_threshold=-1 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
mitosis:
  force_failure_threshold: many
EOF
_assert_fail "mitosis.force_failure_threshold=many is rejected (not integer)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# Both force settings together
_write_config <<'EOF'
mitosis:
  force_on_failure: true
  force_failure_threshold: 3
EOF
_assert_ok "mitosis force_on_failure + force_failure_threshold together is valid" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Tests: Scaling config validation
# ============================================================================

echo ""
echo "========================================"
echo "Scaling config validation tests"
echo "========================================"

_write_config <<'EOF'
scaling:
  spawn_threshold: 3
  max_workers_per_agent: 10
  cooldown_seconds: 30
EOF
_assert_ok "valid scaling config passes" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  spawn_threshold: -1
EOF
_assert_fail "scaling.spawn_threshold=-1 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  spawn_threshold: many
EOF
_assert_fail "scaling.spawn_threshold=many is rejected (not an integer)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  max_workers_per_agent: 0
EOF
_assert_fail "scaling.max_workers_per_agent=0 is rejected (min=1)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  max_workers_per_agent: five
EOF
_assert_fail "scaling.max_workers_per_agent=five is rejected (not an integer)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  cooldown_seconds: -1
EOF
_assert_fail "scaling.cooldown_seconds=-1 is rejected (min=0)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  cooldown_seconds: forever
EOF
_assert_fail "scaling.cooldown_seconds=forever is rejected (not an integer)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  cooldown_seconds: 0
EOF
_assert_ok "scaling.cooldown_seconds=0 is valid (min boundary)" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

_write_config <<'EOF'
scaling:
  spawn_threshold: 1
  max_workers_per_agent: 1
  cooldown_seconds: 0
EOF
_assert_ok "scaling with minimum valid values passes" \
    validate_config_schema "$NEEDLE_CONFIG_FILE"

# ============================================================================
# Summary
# ============================================================================
echo ""
echo "========================================"
echo "Test Summary"
echo "========================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "========================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi
exit 0
