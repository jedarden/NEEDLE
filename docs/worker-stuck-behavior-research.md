# Worker Stuck Behavior Research

**Date:** 2026-03-04
**Bead:** nd-2q7
**Status:** Research Complete

## Executive Summary

This document researches and compares alternative strategies for worker behavior when "stuck" (unable to find work to process). Currently, workers simply wait and retry with exponential backoff until idle timeout. This research explores 8 alternative approaches with detailed analysis of trade-offs.

## Problem Statement

**Scenario:** A NEEDLE worker has exhausted all 7 strands in the strand engine and found no work to process.

**Current Behavior:**
- Increment `consecutive_empty` counter
- Track idle time
- Sleep for `polling_interval` (default: 2s)
- Retry strand engine
- Exit after `idle_timeout` (default: 300s)

**Question:** What alternative strategies could improve efficiency, reduce false positives, or provide better outcomes?

---

## Current Implementation Analysis

### How Workers Get "Stuck"

From `src/runner/loop.sh` and strand engine analysis:

```bash
# Strand engine tries 7 priorities in order:
# 1. Pluck - High-priority beads in current workspace
# 2. Explore - Cross-workspace bead discovery
# 3. Maintenance - System health checks
# 4. Gap Analysis - Missing coverage detection
# 5. Human Alternatives - Unblock HUMAN beads with alternatives
# 6. Knot - Create starvation alert (HUMAN bead)
# 7. Slumber - Idle wait with retry
```

**Worker is "stuck" when:**
- Priority 1 (Pluck): No ready beads in workspace
- Priority 2 (Explore): No workspaces have ready beads
- Priority 3 (Maintenance): All maintenance complete
- Priority 4 (Gap Analysis): No gaps detected
- Priority 5 (Human Alternatives): No HUMAN beads to unblock
- Priority 6 (Knot): Alert already exists or created
- Priority 7 (Slumber): Just waits

### Current Configuration Options

```bash
runner.polling_interval = "2s"        # Wait between retries
runner.idle_timeout = "300s"          # Exit after 5min idle
runner.max_consecutive_empty = 5      # Unused in current implementation
```

### Known Issues

From analysis of worker starvation false positives:
1. **False Alarm Problem** - Workers create starvation alerts when beads actually exist (discovery bugs)
2. **No Diagnostic Logging** - Hard to debug why worker can't find work
3. **No Self-Healing** - Worker doesn't attempt to fix discovery issues
4. **Resource Waste** - Idle workers consume compute/memory while waiting
5. **No Collaboration** - Stuck workers don't signal others or request help

---

## Alternative Strategies

### Alternative 1: Exponential Backoff + Extended Timeout ⏱️

**Approach:** Increase wait time exponentially on consecutive failures.

**Implementation:**
```bash
_needle_worker_loop() {
    local consecutive_empty=0
    local backoff_base=2  # seconds
    local backoff_max=60  # max 1 minute
    local extended_timeout=1800  # 30 minutes instead of 5

    while true; do
        result=$(_needle_strand_engine "$workspace" "$agent")

        if [[ "$result" == "no_work" ]]; then
            ((consecutive_empty++))

            # Calculate exponential backoff: min(base * 2^failures, max)
            local wait_time=$((backoff_base * (2 ** (consecutive_empty - 1))))
            [[ $wait_time -gt $backoff_max ]] && wait_time=$backoff_max

            _needle_debug "No work found (attempt $consecutive_empty), waiting ${wait_time}s"
            sleep "$wait_time"
        else
            consecutive_empty=0
            sleep "$polling_interval"
        fi
    done
}
```

**Pros:**
- Reduces CPU/API load during idle periods
- Allows for temporary issues to resolve (network blips, database locks)
- Still responsive for first few failures
- Standard pattern in distributed systems

**Cons:**
- Delays response to newly created beads
- Maximum backoff may be too long or too short (hard to tune)
- Doesn't address root cause of discovery failures

**Estimated Impact:**
- CPU reduction: ~70% during idle periods
- Discovery latency: +30-60s average when beads created during backoff

**Complexity:** Low (10-15 lines of code)

---

### Alternative 2: Database Health Check + Auto-Repair 🔧

**Approach:** When stuck, verify database integrity and attempt repair before alerting.

**Implementation:**
```bash
_needle_verify_and_repair() {
    local workspace="$1"

    _needle_debug "Running database health check"

    # Check 1: Database file exists
    if [[ ! -f "$workspace/.beads/beads.db" ]]; then
        _needle_warn "No database found - attempting initialization"
        (cd "$workspace" && br sync)
        return 0
    fi

    # Check 2: Database is readable
    if ! (cd "$workspace" && br list --json &>/dev/null); then
        _needle_warn "Database unreadable - attempting repair"
        (cd "$workspace" && br sync --rebuild)
        return 0
    fi

    # Check 3: JSONL vs SQLite consistency
    local jsonl_count=$(jq -s length "$workspace/.beads/issues.jsonl" 2>/dev/null || echo 0)
    local db_count=$(cd "$workspace" && br list --json 2>/dev/null | jq length || echo 0)

    if [[ $((jsonl_count - db_count)) -gt 5 ]]; then
        _needle_warn "DB out of sync (JSONL: $jsonl_count, DB: $db_count) - rebuilding"
        (cd "$workspace" && br sync --rebuild)
        return 0
    fi

    # Check 4: Verify claimed beads are valid
    local orphaned=$(cd "$workspace" && br list --status in_progress --json | \
        jq '[.[] | select(.claim_token != null and .claim_expiry < now)] | length')

    if [[ "$orphaned" -gt 0 ]]; then
        _needle_warn "Found $orphaned orphaned claims - releasing"
        # TODO: Add br command to release expired claims
        return 0
    fi

    return 1  # No issues found or repairs attempted
}

# In main loop, after consecutive_empty threshold
if [[ $consecutive_empty -ge $max_consecutive_empty ]]; then
    if _needle_verify_and_repair "$workspace"; then
        _needle_debug "Database repaired, retrying immediately"
        consecutive_empty=0
        continue
    fi
fi
```

**Pros:**
- Self-healing - fixes common issues automatically
- Reduces false positive starvation alerts
- Provides diagnostic logging
- Addresses root causes identified in analysis docs

**Cons:**
- Adds complexity to worker loop
- Database operations may be expensive
- Risk of masking real problems
- May conflict with concurrent workers

**Estimated Impact:**
- False positive reduction: ~60-80% (based on analysis docs)
- Additional overhead: ~2-5s per health check
- Self-repair success rate: ~40% (estimate)

**Complexity:** Medium (50-80 lines of code)

---

### Alternative 3: Pre-Flight Verification Before Alerting ✅

**Approach:** Before creating starvation alert, verify work truly doesn't exist.

**Implementation:**
```bash
_needle_preflight_check() {
    local workspace="$1"

    _needle_debug "Pre-flight: verifying no work available"

    # Method 1: Direct br list query (bypasses br ready bugs)
    local count
    count=$(cd "$workspace" && br list --status open --json 2>/dev/null | \
            jq '[.[] | select(.claim_token == null or .claim_token == "") |
                      select(.issue_type != "human") |
                      select(.dependency_count == 0)] | length')

    if [[ -n "$count" ]] && [[ "$count" -gt 0 ]]; then
        _needle_warn "Pre-flight FAILED: Found $count claimable beads - discovery bug detected"

        # Emit diagnostic event
        _needle_emit_event "worker.discovery_bug" \
            "workspace=$workspace" \
            "beads_found=$count" \
            "strand_engine_failed=true"

        return 1  # Don't create alert
    fi

    # Method 2: Check using bin/needle-ready workaround tool
    if [[ -x "$NEEDLE_HOME/bin/needle-ready" ]]; then
        local ready_count
        ready_count=$("$NEEDLE_HOME/bin/needle-ready" --workspace "$workspace" --json | jq length)

        if [[ "$ready_count" -gt 0 ]]; then
            _needle_warn "Pre-flight FAILED: needle-ready found $ready_count beads"
            return 1
        fi
    fi

    _needle_debug "Pre-flight PASSED: Confirmed no work available"
    return 0
}

# In Strand 6 (Knot - create alert), before creating HUMAN bead:
if ! _needle_preflight_check "$workspace"; then
    _needle_debug "Skipping starvation alert - pre-flight found work"

    # Trigger database health check instead
    _needle_verify_and_repair "$workspace"

    return 0  # Force retry of strand engine
fi
```

**Pros:**
- Prevents false positive alerts (major pain point from analysis)
- Provides diagnostic data when discrepancies detected
- Low overhead (only runs before alert creation)
- Can trigger auto-repair when bugs detected

**Cons:**
- Adds latency to alert creation (~1-3s)
- Won't fix discovery bugs, just prevents wrong alerts
- Requires maintenance of preflight logic

**Estimated Impact:**
- False positive prevention: ~95% (based on analysis showing beads exist)
- Alert creation delay: +1-3s
- Worker recovery: May self-heal via repair trigger

**Complexity:** Low-Medium (30-40 lines of code)

---

### Alternative 4: Cross-Worker Collaboration Signal 🤝

**Approach:** Stuck workers signal state to other workers; unstuck workers can help.

**Implementation:**
```bash
# State file format: $NEEDLE_STATE/worker-${IDENTIFIER}.status
# Contents: timestamp|status|consecutive_empty|workspace

_needle_emit_worker_status() {
    local status="$1"  # idle, working, stuck, exiting

    echo "$(date +%s)|$status|$consecutive_empty|$NEEDLE_WORKSPACE" \
        > "$NEEDLE_STATE/worker-${NEEDLE_IDENTIFIER}.status"
}

_needle_check_peer_workers() {
    local stuck_count=0
    local working_count=0

    for status_file in "$NEEDLE_STATE"/worker-*.status; do
        [[ ! -f "$status_file" ]] && continue

        IFS='|' read -r timestamp status empty workspace < "$status_file"

        # Ignore stale status (>60s old)
        local age=$(($(date +%s) - timestamp))
        [[ $age -gt 60 ]] && continue

        case "$status" in
            stuck) ((stuck_count++)) ;;
            working) ((working_count++)) ;;
        esac
    done

    # If ALL workers stuck, likely real starvation
    if [[ $stuck_count -gt 0 ]] && [[ $working_count -eq 0 ]]; then
        _needle_debug "All $stuck_count workers stuck - likely real starvation"
        return 0  # Proceed with alert
    fi

    # If some workers working, this is transient or discovery bug
    if [[ $working_count -gt 0 ]]; then
        _needle_debug "$working_count workers active - skipping alert"
        return 1  # Don't alert
    fi

    return 0
}

# In main loop:
if [[ $consecutive_empty -ge $max_consecutive_empty ]]; then
    _needle_emit_worker_status "stuck"

    if ! _needle_check_peer_workers; then
        _needle_debug "Other workers active - assuming transient issue"
        consecutive_empty=0  # Reset and retry
    fi
else
    _needle_emit_worker_status "working"
fi
```

**Pros:**
- Reduces false positives (if others working, not real starvation)
- Provides visibility into worker pool state
- Enables future peer-to-peer features (work stealing, load balancing)
- Low overhead (file writes only when state changes)

**Cons:**
- Requires shared state directory (already exists: `NEEDLE_STATE`)
- File I/O overhead for status updates
- Stale status files need cleanup
- May delay legitimate alerts if peer status outdated

**Estimated Impact:**
- False positive reduction: ~30-50% (transient issues)
- Overhead: ~10-20ms per status check
- State file size: ~100 bytes per worker

**Complexity:** Medium (60-80 lines of code)

---

### Alternative 5: Graceful Exit + Restart on New Work 🔄

**Approach:** Exit cleanly when idle; systemd/supervisor restarts when new beads created.

**Implementation:**
```bash
# In loop: after idle timeout
if [[ $idle_seconds -ge $idle_timeout ]]; then
    _needle_emit_event "worker.idle_exit" \
        "reason=no_work_available" \
        "idle_duration=${idle_seconds}s"

    exit 0  # Clean exit (not failure)
fi

# Systemd unit: needle-worker@.service
# [Unit]
# Description=NEEDLE Worker %i
#
# [Service]
# Type=simple
# ExecStart=/usr/local/bin/needle run --agent %i
# Restart=on-success          # Restart on exit 0
# RestartSec=30               # Wait 30s before restart
# SuccessExitStatus=0 3       # Exit 0 or 3 = success
#
# [Install]
# WantedBy=multi-user.target

# File watcher (inotifywait): watches .beads/issues.jsonl
# When modified -> systemctl restart needle-worker@*
```

**Alternative: Git Hook Trigger**
```bash
# .git/hooks/post-commit or post-receive
#!/bin/bash
if git diff-tree --name-only HEAD | grep -q '.beads/issues.jsonl'; then
    # New beads committed, wake workers
    systemctl restart needle-worker@* 2>/dev/null || true
fi
```

**Pros:**
- Zero resource usage when idle (workers exit)
- Clean separation of concerns (systemd handles lifecycle)
- Works well with CI/CD (run workers on push, exit when done)
- Standard pattern for batch processing

**Cons:**
- Requires systemd or equivalent supervisor
- Startup latency when new work arrives (10-30s)
- May miss rapid bead creation if restart delay too long
- Needs file watcher or hook integration

**Estimated Impact:**
- Resource savings: 100% when idle (workers not running)
- Response latency: +30-60s (restart time + discovery)
- Complexity: Requires systemd integration

**Complexity:** Medium-High (supervisor integration, file watching)

---

### Alternative 6: Diagnostic Logging + Human Report 📊

**Approach:** When stuck, collect extensive diagnostics and present to human.

**Implementation:**
```bash
_needle_generate_starvation_report() {
    local workspace="$1"
    local report_file="/tmp/needle-starvation-${NEEDLE_IDENTIFIER}-$(date +%s).md"

    cat > "$report_file" <<EOF
# Worker Starvation Diagnostic Report

**Worker:** ${NEEDLE_IDENTIFIER}
**Workspace:** ${workspace}
**Timestamp:** $(date -Iseconds)
**Uptime:** ${SECONDS}s
**Consecutive Empty:** ${consecutive_empty}

## Database Status

\`\`\`
$(cd "$workspace" && br status 2>&1)
\`\`\`

## Open Beads

\`\`\`
$(cd "$workspace" && br list --status open --limit 20 2>&1)
\`\`\`

## Ready Beads (br ready)

\`\`\`
$(cd "$workspace" && br ready --unassigned --limit 20 2>&1)
\`\`\`

## Fallback Query (manual filter)

\`\`\`
$(cd "$workspace" && br list --status open --json 2>/dev/null | \
  jq '[.[] | select(.claim_token == null) | select(.issue_type != "human")] | .[0:10]')
\`\`\`

## Database Files

\`\`\`
$(ls -lh "$workspace/.beads/" 2>&1)
\`\`\`

## Environment

- PATH: $PATH
- br location: $(which br 2>/dev/null || echo 'NOT FOUND')
- Working directory: $(pwd)
- NEEDLE_HOME: $NEEDLE_HOME

## Recent Events

\`\`\`
$(tail -20 "$NEEDLE_LOG_FILE" 2>/dev/null || echo 'No log file')
\`\`\`

## Strand Engine Results

$(cat "$NEEDLE_STATE/strand-results-${NEEDLE_SESSION}.log" 2>/dev/null || echo 'No strand results')

## Recommendations

EOF

    # Auto-analyze and add recommendations
    local open_count=$(cd "$workspace" && br list --status open --json 2>/dev/null | jq length)

    if [[ "$open_count" -gt 0 ]]; then
        cat >> "$report_file" <<EOF
⚠️ **LIKELY FALSE POSITIVE** - Found $open_count open beads

**Possible Causes:**
- Discovery mechanism bug (br ready vs br list discrepancy)
- Workspace path mismatch
- Database query filter too restrictive
- Race condition in claim/release

**Suggested Actions:**
1. Run: \`./bin/needle-ready --workspace $workspace\` to verify
2. Check: Database health with \`br status\`
3. Test: Manually claim bead with \`br update <id> --claim\`
4. Consider: Database rebuild with \`br sync --rebuild\`
EOF
    else
        cat >> "$report_file" <<EOF
✅ **LIKELY REAL STARVATION** - No open beads found

**Suggested Actions:**
1. Review closed beads: Check if all work complete
2. Create new beads: If more work needed
3. Check dependencies: Verify no circular blocks
4. Review deferred beads: May need to undefer
EOF
    fi

    echo "$report_file"
}

# In Strand 6, instead of simple alert:
report=$(_needle_generate_starvation_report "$workspace")

br create --type human --priority 1 \
  --title "WORKER STARVATION: $NEEDLE_IDENTIFIER stuck" \
  --description "Worker unable to find work. Diagnostic report attached.

See: $report

**Quick Stats:**
- Workspace: $workspace
- Uptime: ${SECONDS}s
- Consecutive empty: $consecutive_empty

$(cat "$report")" \
  --label starvation,diagnostic
```

**Pros:**
- Provides all context needed for debugging
- Auto-analyzes likely cause (false positive vs real)
- Creates actionable recommendations
- Preserves diagnostic state for post-mortem

**Cons:**
- Generates large reports (may be 5-10KB)
- File system overhead
- Doesn't fix issues, just reports
- May overwhelm human with data

**Estimated Impact:**
- Debug time reduction: ~80% (all info in one place)
- Report generation time: ~2-5s
- Storage: ~5-10KB per report

**Complexity:** Medium (80-100 lines of code)

---

### Alternative 7: Dynamic Strand Priority Adjustment 🎯

**Approach:** Learn from failures; adjust strand priorities to retry failed strands less.

**Implementation:**
```bash
# Track strand success rates
declare -A strand_success_count
declare -A strand_attempt_count

_needle_strand_engine() {
    local workspace="$1"
    local agent="$2"

    # Initialize counters
    for i in {1..7}; do
        : ${strand_attempt_count[$i]:=0}
        : ${strand_success_count[$i]:=0}
    done

    # Calculate dynamic priorities based on success rate
    local -a strand_order=(1 2 3 4 5 6 7)

    # If we have history, reorder by success rate
    if [[ ${strand_attempt_count[1]} -gt 10 ]]; then
        strand_order=($(
            for strand in {1..7}; do
                local attempts=${strand_attempt_count[$strand]}
                local successes=${strand_success_count[$strand]}
                local rate=0
                [[ $attempts -gt 0 ]] && rate=$((successes * 100 / attempts))
                echo "$rate $strand"
            done | sort -rn | awk '{print $2}'
        ))

        _needle_debug "Dynamic strand order: ${strand_order[*]}"
    fi

    # Try strands in calculated order
    for strand_num in "${strand_order[@]}"; do
        ((strand_attempt_count[$strand_num]++))

        if "_needle_strand_${strand_num}" "$workspace" "$agent"; then
            ((strand_success_count[$strand_num]++))
            return 0
        fi
    done

    return 1
}
```

**Pros:**
- Optimizes strand order over time
- Reduces latency by trying successful strands first
- Self-tuning based on workspace characteristics
- No configuration needed

**Cons:**
- Adds complexity to strand engine
- May create local maxima (skip strands that could succeed)
- Needs warm-up period (first 10+ attempts)
- Success rate may change over time (stale)

**Estimated Impact:**
- Latency reduction: ~20-40% after warm-up
- CPU reduction: ~15-25% (fewer failed strand attempts)
- Memory: +200 bytes per worker (counters)

**Complexity:** Medium (50-70 lines of code)

---

### Alternative 8: Hybrid: Smart Backoff + Pre-Flight + Diagnostics 🌟

**Approach:** Combine best elements from alternatives 1, 3, and 6.

**Implementation:**
```bash
_needle_handle_no_work() {
    local workspace="$1"
    local consecutive_empty="$2"

    # Phase 1: Immediate retries (0-2 failures)
    if [[ $consecutive_empty -le 2 ]]; then
        sleep "$polling_interval"
        return 0
    fi

    # Phase 2: Database health check (3-4 failures)
    if [[ $consecutive_empty -le 4 ]]; then
        if _needle_verify_and_repair "$workspace"; then
            _needle_debug "Database repaired, resetting counter"
            return 1  # Signal reset counter
        fi
        sleep $((polling_interval * 2))
        return 0
    fi

    # Phase 3: Pre-flight before alerting (5 failures)
    if [[ $consecutive_empty -eq 5 ]]; then
        if ! _needle_preflight_check "$workspace"; then
            _needle_warn "Pre-flight found work - discovery bug detected"

            # Trigger aggressive repair
            _needle_verify_and_repair "$workspace"

            return 1  # Reset counter, force retry
        fi

        # Pre-flight passed - likely real starvation
        _needle_debug "Pre-flight confirmed no work - proceeding to alert"
    fi

    # Phase 4: Extended backoff (6-10 failures)
    if [[ $consecutive_empty -le 10 ]]; then
        local backoff=$((polling_interval * (2 ** (consecutive_empty - 5))))
        [[ $backoff -gt 60 ]] && backoff=60

        _needle_debug "Extended backoff: ${backoff}s"
        sleep "$backoff"
        return 0
    fi

    # Phase 5: Generate diagnostic report and alert (11+ failures)
    if [[ $consecutive_empty -eq 11 ]]; then
        local report=$(_needle_generate_starvation_report "$workspace")

        # Create alert with full diagnostics
        _needle_create_starvation_alert "$workspace" "$report"
    fi

    # Phase 6: Wait for idle timeout
    sleep 60
    return 0
}

# In main loop:
result=$(_needle_strand_engine "$workspace" "$agent")

if [[ "$result" == "no_work" ]]; then
    ((consecutive_empty++))

    if _needle_handle_no_work "$workspace" "$consecutive_empty"; then
        : # Continue normally
    else
        consecutive_empty=0  # Reset signaled
    fi
else
    consecutive_empty=0
fi
```

**Phases Explained:**
1. **0-2 failures:** Fast retry (2s) - likely transient issue
2. **3-4 failures:** Health check + moderate backoff (4s) - attempt self-heal
3. **5 failures:** Pre-flight verification - confirm no false positive
4. **6-10 failures:** Exponential backoff (8-60s) - reduce load, wait for new work
5. **11 failures:** Generate diagnostics + create alert - escalate to human
6. **12+ failures:** Long wait (60s) until timeout - minimize resource usage

**Pros:**
- Progressive escalation - handles both transient and permanent issues
- Self-healing before alerting
- Prevents false positives
- Provides rich diagnostics when alerting
- Optimizes resource usage over time

**Cons:**
- Most complex implementation
- Many configuration knobs to tune
- May delay legitimate alerts (phases 1-4 take ~90s)
- Hard to debug phased behavior

**Estimated Impact:**
- False positive prevention: ~85-95%
- Self-repair success: ~40-60%
- Resource usage: -60% during idle
- Alert creation delay: +60-120s

**Complexity:** High (150-200 lines of code)

---

## Comparison Matrix

| Alternative | False Positive Prevention | Self-Healing | Resource Efficiency | Response Latency | Complexity | Recommended? |
|-------------|--------------------------|--------------|---------------------|------------------|------------|--------------|
| **1. Exponential Backoff** | ❌ None | ❌ No | ✅ Good (-70%) | ⚠️ +30-60s | Low | ⭐ Good for CI/CD |
| **2. Health Check + Repair** | ✅ Moderate (60%) | ✅ Yes | ⚠️ Neutral | ⚠️ +2-5s | Medium | ⭐⭐ Good standalone |
| **3. Pre-Flight Verification** | ✅✅ Excellent (95%) | ⚠️ Triggers repair | ✅ Good (low overhead) | ✅ Minimal (+1-3s) | Low-Med | ⭐⭐⭐ **Highly Recommended** |
| **4. Worker Collaboration** | ✅ Moderate (30-50%) | ❌ No | ✅ Good | ✅ Minimal | Medium | ⚠️ Future enhancement |
| **5. Graceful Exit** | ❌ None | ❌ No | ✅✅ Excellent (-100%) | ❌ Poor (+30-60s) | Med-High | ⭐ Good for batch jobs |
| **6. Diagnostic Logging** | ❌ None | ❌ No | ⚠️ Neutral | ⚠️ +2-5s | Medium | ⭐⭐ Good for debugging |
| **7. Dynamic Priorities** | ❌ None | ⚠️ Indirect | ✅ Good (-15-25%) | ✅ Improved (-20-40%) | Medium | ⚠️ Optimization only |
| **8. Hybrid Approach** | ✅✅ Excellent (85-95%) | ✅ Yes | ✅ Good (-60%) | ⚠️ +60-120s | High | ⭐⭐⭐ **Best comprehensive** |

---

## Recommendations

### Immediate Implementation (Priority 0)

**Implement Alternative 3: Pre-Flight Verification**

**Rationale:**
- Analysis documents show **all recent starvation alerts were false positives**
- Pre-flight check prevents 95% of false alerts with minimal overhead
- Low complexity, high impact
- Can be implemented in ~1 hour

**Code Location:** `src/strands/knot.sh` (Strand 6 - Alert creation)

**Estimated Effort:** 1-2 hours

---

### Short-Term Enhancement (Priority 1)

**Add Alternative 2: Database Health Check**

**Rationale:**
- Root cause analysis identified database sync issues
- Self-healing reduces human intervention
- Complements pre-flight check
- Medium complexity, high value

**Code Location:** `src/runner/loop.sh` (before consecutive_empty threshold)

**Estimated Effort:** 3-4 hours

---

### Medium-Term Optimization (Priority 2)

**Implement Alternative 1: Exponential Backoff**

**Rationale:**
- Reduces resource usage during legitimately idle periods
- Standard distributed systems pattern
- Low complexity, proven technique

**Code Location:** `src/runner/loop.sh` (main loop wait logic)

**Estimated Effort:** 1-2 hours

---

### Long-Term Consideration (Priority 3)

**Research Alternative 4: Worker Collaboration**

**Rationale:**
- Enables advanced features (work stealing, load balancing)
- Reduces false positives for multi-worker deployments
- Foundation for distributed NEEDLE

**Code Location:** New module `src/runner/collaboration.sh`

**Estimated Effort:** 8-12 hours (design + implementation)

---

### Optional Enhancements

**Alternative 6: Diagnostic Logging**
- Use when debugging specific issues
- Can be feature-flagged for production

**Alternative 7: Dynamic Priorities**
- Optimization after base system stable
- A/B test to measure actual benefit

**Alternative 5: Graceful Exit**
- Best for CI/CD environments
- Document as deployment pattern, not default

---

## Implementation Roadmap

### Phase 1: Prevent False Positives (Week 1)
```bash
# Bead: Implement pre-flight verification
- Add _needle_preflight_check() to src/lib/verify.sh
- Integrate into src/strands/knot.sh before alert creation
- Add tests: test_preflight_detection.sh
- Metrics: Track false positive rate reduction
```

### Phase 2: Self-Healing (Week 2)
```bash
# Bead: Implement database health checks
- Add _needle_verify_and_repair() to src/lib/verify.sh
- Integrate into loop after consecutive_empty threshold
- Add tests: test_auto_repair.sh
- Metrics: Track repair success rate
```

### Phase 3: Resource Optimization (Week 3)
```bash
# Bead: Add exponential backoff
- Modify _needle_worker_loop() in src/runner/loop.sh
- Add configuration: runner.backoff_base, runner.backoff_max
- Add tests: test_backoff_behavior.sh
- Metrics: Track CPU/memory usage reduction
```

### Phase 4: Enhanced Diagnostics (Week 4)
```bash
# Bead: Diagnostic report generation
- Add _needle_generate_starvation_report() to src/lib/diagnostics.sh
- Enhance starvation alerts with reports
- Add tests: test_diagnostic_generation.sh
- Metrics: Track human resolution time
```

---

## Success Metrics

### Key Performance Indicators (KPIs)

1. **False Positive Rate**
   - Current: ~100% (all recent alerts were false positives)
   - Target: <5%
   - Measurement: `false_alerts / total_alerts`

2. **Self-Repair Success Rate**
   - Current: 0% (no auto-repair)
   - Target: >50%
   - Measurement: `repaired_issues / detected_issues`

3. **Idle Resource Usage**
   - Current: 100% (workers run continuously)
   - Target: <40%
   - Measurement: `cpu_usage_idle / cpu_usage_working`

4. **Alert Response Time**
   - Current: Immediate (but mostly wrong)
   - Target: <2 minutes
   - Measurement: `time(issue_detected -> alert_created)`

5. **Human Intervention Rate**
   - Current: High (every false positive)
   - Target: <10% of stuck scenarios
   - Measurement: `human_interventions / stuck_events`

---

## Testing Strategy

### Unit Tests
```bash
# test_preflight_verification.sh
test_preflight_detects_open_beads()
test_preflight_allows_alert_when_truly_empty()
test_preflight_handles_database_errors()

# test_health_check.sh
test_repair_rebuilds_corrupted_database()
test_repair_releases_orphaned_claims()
test_repair_syncs_jsonl_to_db()

# test_backoff.sh
test_backoff_exponential_growth()
test_backoff_respects_maximum()
test_backoff_resets_on_work_found()
```

### Integration Tests
```bash
# test_worker_stuck_scenarios.sh
test_false_positive_prevented()
test_transient_issue_recovers()
test_permanent_starvation_alerts()
test_database_corruption_repairs()
```

### Load Tests
```bash
# test_resource_usage.sh
test_idle_cpu_usage()
test_idle_memory_usage()
test_backoff_reduces_api_calls()
```

---

## Related Documentation

- `docs/worker-starvation-alternatives.md` - Previous research
- `docs/worker-starvation-false-alarm-analysis.md` - nd-dd6 analysis
- `docs/worker-starvation-alert-nd-390-analysis.md` - nd-390 analysis
- `src/runner/loop.sh` - Current implementation
- `src/strands/engine.sh` - Strand engine implementation

---

## Conclusion

**Recommended Approach:** Implement **Alternative 8 (Hybrid)** in phases:
1. Start with **Alternative 3** (pre-flight) - quick win, prevents false positives
2. Add **Alternative 2** (health check) - self-healing capability
3. Enhance with **Alternative 1** (backoff) - resource optimization
4. Polish with **Alternative 6** (diagnostics) - better debugging

**Total Estimated Effort:** 2-3 weeks for full implementation

**Expected Outcomes:**
- **95% reduction in false positive alerts**
- **50-60% self-healing success rate**
- **60-70% reduction in idle resource usage**
- **Better human experience** (fewer meaningless alerts)

This phased approach balances immediate impact (pre-flight check) with long-term robustness (hybrid system).
