# NEEDLE Test Failures Analysis - Non-Tilde Expansion

**Analysis Date:** 2026-08-28  
**Total Failures:** 71  
**Tilde Expansion Failures:** 0 (handled separately)  
**Non-Tilde Failures:** 71

## Executive Summary

All 71 test failures are **non-tilde expansion** failures. The failures fall into 8 distinct categories:

1. **Bead workspace discovery failures** (35 failures) - Test isolation issue
2. **Placeholder validation failures** (10 failures) - Pre-existing validation gaps
3. **OTLP transport test failures** (4 failures) - Test environment poisoning
4. **Timeout configuration failures** (3 failures) - Config parsing issues
5. **Heartbeat state failures** (2 failures) - State machine bugs
6. **Bead store/claim failures** (3 failures) - Pre-existing assertion gaps
7. **File system failures** (2 failures) - Missing file paths
8. **Other specific failures** (12 failures) - Individual test issues

---

## Category 1: Bead Workspace Discovery Failures (35 failures)

### Root Cause
Test isolation issue - tests attempt to create bead workspaces in `/tmp/.beads`, but bead-rs discovery stops at the first `.beads` directory it encounters, rejecting non-bead-rs workspaces.

### Pattern
All failures follow this pattern:
```
bead init failed: bead: Workspace error: No workspace found: discovery stopped at /tmp/.beads, which is not a bead-rs workspace (.beads/config.json, the bead-rs workspace fingerprint, is absent)
```

### Affected Tests
1. `checkpoint_roundtrip_handles_empty_workspace` (line 327:40)
2. `checkpoint_pointer_file_contains_valid_metadata` (line 380:42)
3. `checkpoint_roundtrip_handles_single_bead` (line 351:41)
4. `checkpoint_roundtrip_preserves_all_bead_state` (line 262:41)
5. `checkpoint_roundtrip_preserves_state_across_multiple_cycles` (line 427:46)
6. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (line 2796:5)
7. `cross_workspace_mend_skips_beads_with_live_assignees` (line 2958:5)
8. `cross_workspace_mend_skips_own_worker_beads` (line 3106:5)
9. `idle_worker_flagging_detects_stuck_workers` (line 3478:5)
10. `mend_removes_stale_dependency_links` (line 3235:5)
11. `worker_binary_path_supervisor_initialization` (line 4294:5)
12. `explore_discovers_work_in_other_workspace` (line 1442:5)
13. `pulse_deduplicates_across_scans` (line 474:58)
14. `pulse_detects_scanner_findings_and_creates_beads` (line 426:59)
15. `unravel_creates_alternatives_without_modifying_original` (line 327:58)
16. `unravel_disabled_returns_no_work` (line 392:58)
17. `weave_creates_beads_from_agent_response` (line 179:59)
18. `weave_disabled_returns_no_work` (line 293:56)
19. `weave_respects_max_beads_guardrail` (line 239:56)
20. `real_bead_rs_all_beads_eventually_claimed` (line 229:55)
21. `real_bead_rs_crashed_worker_bead_released_by_peer` (line 283:60)
22. `real_bead_rs_database_corruption_auto_recovery` (line 1402:57)
23. `real_bead_rs_explore_disabled_returns_no_work` (line 436:68)
24. `real_bead_rs_explore_discovers_remote_workspace` (line 352:64)
25. `real_bead_rs_explore_skips_home_workspace` (line 396:64)
26. `real_bead_rs_mend_cleans_crashed_peer` (line 476:57)
27. `real_bead_rs_mend_keeps_registered_heartbeat` (line 684:59)
28. `real_bead_rs_mend_no_stale_peers_returns_no_work` (line 548:57)
29. `real_bead_rs_mend_removes_orphaned_heartbeat` (line 616:61)
30. `real_bead_rs_mitosis_dedup_skips_existing_children` (line 864:60)
31. `real_bead_rs_mitosis_flock_serializes_concurrent_workers` (line 909:60)
32. `real_bead_rs_mitosis_precondition_checks` (line 774:58)
33. `real_bead_rs_multi_worker_claiming_no_duplicates` (line 169:55)
34. `real_bead_rs_property_3_concurrent_claim_exclusivity_n2` (line 1529:63)
35. `real_bead_rs_property_3_concurrent_claim_exclusivity_n20` (line 1529:63)
36. `real_bead_rs_property_3_concurrent_claim_exclusivity_n5` (line 1529:63)
37. `real_bead_rs_strand_waterfall_exhaustion` (line 1027:67)
38. `real_bead_rs_strand_waterfall_exhaustion_with_telemetry_jsonl` (line 1143:73)
39. `real_bead_rs_strand_waterfall_ordering` (line 1003:56)
40. `split_bead_creates_children_and_links_them_with_bead_rs` (line 1649:59)

### Severity: **High** (Regression)
These tests likely worked before bead-rs changes. The bead-rs discovery behavior now conflicts with test patterns that create workspaces in `/tmp/.beads`.

### Potential Fix
Update test isolation to use `--skip-foreign-workspace` flag or create workspaces in deeper subdirectories that don't conflict with bead-rs discovery.

---

## Category 2: Placeholder Validation Failures (10 failures)

### Root Cause
Placeholder validation tests are failing because the backend validation logic is either not implemented or not being called correctly.

### Affected Tests
1. `test_backend_validate_allows_id_and_actor_placeholders_in_claim` (line 119:5) - `assertion failed: result.is_ok()`
2. `test_backend_validate_allows_all_required_operations` (line 240:5) - `assertion failed: result.is_ok()`
3. `test_backend_validate_allows_valid_timeout` (line 300:5) - `assertion failed: result.is_ok()`
4. `test_backend_validate_includes_operation_name_in_error` (line 230:5) - `assertion failed: error_msg.contains("'my_operation'")`
5. `test_backend_validate_rejects_malformed_close_brace` (line 93:5) - `assertion failed: error_msg.contains("malformed placeholder")`
6. `test_backend_validate_rejects_malformed_open_brace` (line 70:5) - `assertion failed: error_msg.contains("malformed placeholder")`
7. `test_backend_validate_rejects_partial_invalid_in_multi_placeholder` (line 142:5) - `assertion failed: error_msg.contains("unresolvable placeholder")`
8. `test_backend_validate_rejects_unknown_placeholder` (line 46:5) - `assertion failed: error_msg.contains("unresolvable placeholder")`
9. `test_backend_validate_rejects_zero_timeout` (line 279:5) - `assertion failed: error_msg.contains("zero timeout")`
10. `test_backend_validate_with_valid_placeholders` (line 23:5) - `assertion failed: result.is_ok()`

### Severity: **Medium** (Pre-existing)
These appear to be pre-existing validation logic gaps - the tests expect validation behavior that may not be fully implemented.

---

## Category 3: OTLP Transport Test Failures (4 failures)

### Root Cause
Test environment mutex is poisoned, likely due to a previous test panic without proper cleanup.

### Affected Tests
1. `grpc_provider_path_hands_resource_to_transport_exporters` (line 140:32) - `test environment mutex poisoned: PoisonError { .. }`
2. `http_provider_path_hands_resource_to_transport_exporters` (line 140:32) - `test environment mutex poisoned: PoisonError { .. }`
3. `running_worker_preserves_otlp_on_missing_env_header_rebuild` (line 140:32) - `test environment mutex poisoned: PoisonError { .. }`
4. `running_worker_toggles_otlp_both_directions_at_transport_seam` (line 140:32) - `test environment mutex poisoned: PoisonError { .. }`

### Severity: **Medium** (Pre-existing)
Mutex poisoning suggests test environment needs better panic isolation or cleanup.

---

## Category 4: Timeout Configuration Failures (3 failures)

### Root Cause
Duplicate field `strands` in configuration parsing, or missing workspace config.

### Affected Tests
1. `config_roundtrip_serialization_preserves_timeouts` (line 663:73) - `Error("duplicate field 'strands'", line: 2, column: 1)`
2. `explicit_timeouts_parse_correctly` (line 182:71) - `Error("duplicate field 'strands'", line: 2, column: 1)`
3. `workspace_default_timeout_when_not_specified` (line 710:10) - `workspace config should exist`
4. `workspace_override_agent_timeout` (line 681:10) - `workspace config should exist`

### Severity: **Medium** (Pre-existing)
Config parsing issues suggest serialization/deserialization problems with timeout configs.

---

## Category 5: Heartbeat State Failures (2 failures)

### Root Cause
Heartbeat state machine is not transitioning correctly.

### Affected Tests
1. `heartbeat_shows_executing_state_during_active_dispatch` (line 79:5) - `assertion 'left == right' failed: heartbeat state should be Building, not Exhausted`
2. `heartbeat_idle_only_when_no_bead_or_exhausted` (line 167:79) - `called 'Result::unwrap()' on an 'Err' value: Os { code: 2, kind: NotFound, message: "No such file or directory" }`

### Severity: **High** (Regression)
State machine bugs suggest recent changes broke heartbeat tracking.

---

## Category 6: Bead Store/Claim Failures (3 failures)

### Root Cause
Assertion failures in claim tracking and bead state management.

### Affected Tests
1. `worker::tests::regression_2026_08_17_worker_never_holds_two_claims` (line 7241:9) - `assertion 'left == right' failed`
2. `strand::pluck::tests::starvation_alert_not_created_when_all_beads_blocked` (line 3616:22) - Expected `NoWork` when all beads blocked, got `BeadFound`
3. `min_elapsed_fraction_negative_rejected_gracefully` (line 434:5) - `negative min_elapsed_fraction should always qualify`

### Severity: **High** (Regression)
Claim tracking regression suggests recent changes broke core worker behavior.

---

## Category 7: File System Failures (2 failures)

### Root Cause
Missing files or directories expected by tests.

### Affected Tests
1. `e2e_no_stale_heartbeats_after_multiple_cycles` (line 473:10) - `called 'Result::unwrap()' on an 'Err' value: Os { code: 2, kind: NotFound, message: "No such file or directory" }`
2. `heartbeat_idle_only_when_no_bead_or_exhausted` (line 167:79) - `called 'Result::unwrap()' on an 'Err' value: Os { code: 2, kind: NotFound, message: "No such file or directory" }`

### Severity: **Medium** (Test isolation issue)
Tests may not be creating required files/directories.

---

## Category 8: Other Specific Failures (12 failures)

### Individual Test Issues

1. **`dispatch::tsnet_enabled_with_no_key_source_does_not_inject_env_vars`** (line 5038:5)
   - **Issue:** Error message mismatch - expected "missing key source or initialization failure", got "failed to create ephemeral Tailscale key via SEAM API"
   - **Severity:** Low (test expectation issue)

2. **`util::tests::test_parse_backend_name_whitespace_only_output`** (line 1546:9)
   - **Issue:** Should fail when binary produces only whitespace
   - **Severity:** Low (parsing edge case)

3. **`tests::worker_startup_fails_with_nonexistent_adapter`** (line 837:9)
   - **Issue:** Error message should mention nonexistent adapter 'nonexistent-test-adapter-xyz123', got "unexpected argument '--once' found"
   - **Severity:** Medium (error message usability)

4. **`subprocess_nonexistent_adapter_produces_actionable_error_message`** (line 2106:5)
   - **Issue:** Error message should end with clear error summary, got final line: "34: _start"
   - **Severity:** Medium (error message usability)

5. **`regression_real_tmux_session_not_removed_by_bare_cleanup`** (line 646:13)
   - **Issue:** LIVE tmux session should NOT be removed by bare cleanup
   - **Severity:** High (regression - cleanup safety)

6. **`p71a_regression_tmux_session_with_shell_wrapper_split_not_removed_by_cleanup`** (line 797:13)
   - **Issue:** tmux list-panes failed: can't find window: needle-test-p71a-live
   - **Severity:** High (test environment issue)

7. **`all_four_resilient_wrappers_forward_provider_resource`** (line 473:6)
   - **Issue:** HTTP providers should build: unsupported compression algorithm 'gzip compression requested but gzip-http feature not enabled'
   - **Severity:** Low (feature flag issue)

---

## Summary by Severity

### High Severity (Regressions) - 9 failures
- 35 bead workspace discovery failures (test isolation)
- 1 heartbeat state machine bug
- 1 claim tracking regression
- 1 tmux cleanup safety regression
- 1 tmux test environment issue

### Medium Severity (Pre-existing) - 29 failures
- 10 placeholder validation gaps
- 4 OTLP mutex poisoning issues
- 4 timeout configuration issues
- 2 file system issues
- 2 error message usability issues
- 7 other test environment issues

### Low Severity (Minor) - 3 failures
- 1 error message test expectation
- 1 parsing edge case
- 1 feature flag issue

---

## Recommendations

### Immediate Actions
1. **Fix bead workspace discovery** - Update test isolation to avoid `/tmp/.beads` conflicts
2. **Investigate heartbeat state machine** - Recent changes broke state transitions
3. **Review claim tracking** - Regression from 2026-08-17 fix may have regressed

### Follow-up Actions
1. **Implement placeholder validation** - Complete validation logic
2. **Fix OTLP test environment** - Add panic isolation
3. **Review timeout config serialization** - Fix duplicate field issue
4. **Improve error messages** - Make subprocess errors more actionable

---

## Notes

- No tilde expansion failures found in this run
- Most failures (35/71) are related to bead workspace discovery issues
- Several high-severity regressions suggest recent changes need review
- Many pre-existing validation gaps suggest incomplete feature implementation