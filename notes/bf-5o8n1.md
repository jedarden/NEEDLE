# Task Already Completed: Redirect Pluck Starvation to Telemetry

## Status: ALREADY IMPLEMENTED

This task was already completed in prior work sessions. The implementation satisfies all acceptance criteria:

## Acceptance Criteria Status

### ✅ Starvation detection emits telemetry event
- **Location**: `src/strand/pluck.rs` lines 491-498
- **Event**: `PluckStarvationDetected`
- **Fields**: workspace, open_count, excluded_count, candidate_exclusion_reasons
- **Commit**: `dab3448 feat(needle-bf-31dnh): emit PluckStarvationDetected telemetry when no candidates`

### ✅ No bead written to target workspace
- **Verification**: `grep -n "create_bead\|create" src/strand/pluck.rs | grep -v "test\|mock\|async fn"` shows no bead creation in production code
- The only `create_bead` calls are in test mocks (MemoryStore, UnfilteredStore, FailingStore)

### ✅ Persistent records in NEEDLE workspace only
- **Location**: `src/strand/pluck.rs` lines 196-248, 501-513
- **File**: `~/.needle/state/starvation-records.jsonl`
- **Feature**: Optional via `PluckStrand::with_persistent_records()` constructor
- **Commit**: `98050ef feat(needle-bf-qn3f3): add optional persistent starvation record in NEEDLE workspace`

### ✅ Unit tests verify starvation scenarios
- **Test Count**: 7 tests covering all scenarios
- **Test Names**:
  1. `starvation_when_all_beads_excluded_by_labels_emits_telemetry`
  2. `starvation_when_all_beads_have_stale_assignees_emits_telemetry`
  3. `starvation_when_queue_is_genuinely_empty_emits_telemetry`
  4. `starvation_mixed_label_and_assignee_exclusions_emits_telemetry`
  5. `starvation_persistent_record_written_to_needle_workspace`
  6. `starvation_persistent_record_disabled_when_flag_false`
  7. `starvation_persistent_record_not_written_to_target_workspace`

## Implementation History

1. **Commit `d3b9042`** (bf-5jq9a): Track exclusion reasons during PluckStrand filtering
2. **Commit `9dd1fb8`** (bf-nxowc): Add telemetry emitter field to PluckStrand
3. **Commit `dab3448`** (bf-31dnh): Emit PluckStarvationDetected telemetry when no candidates
4. **Commit `4adbb62`** (bf-3r2gz): Add tests for starvation scenarios
5. **Commit `98050ef`** (bf-qn3f3): Add optional persistent starvation record in NEEDLE workspace

## ADR-002 Compliance

The implementation fully satisfies ADR-002 decision 1:
> "Redirect Pluck's starvation self-diagnostic to NEEDLE's own telemetry. Never write it as a bead into the scanned workspace. Emit a structured `pluck.starvation_detected` telemetry event... If a persistent, actionable record is wanted, file it as a bead in **NEEDLE's own** workspace, never the target's."

## Conclusion

No further work required. The task was completed across multiple prior work sessions and is production-ready.
