# Claim Module Unit Test Results

**Date:** 2026-07-14  
**Bead:** bf-xa5gr  
**Task:** Test claim module unit tests

## Summary

All 26 unit tests in the claim module passed successfully.

## Test Coverage

The claim module tests cover:

### Core Claim Tests (11 tests)
- `claim_next_all_excluded_returns_no_candidates`
- `claim_next_empty_candidates_returns_no_candidates`
- `claim_next_happy_path_returns_claimed`
- `claim_next_not_claimable_skips_to_next`
- `claim_next_race_lost_tries_next_candidate`
- `claim_one_happy_path`
- `exclusion_set_prevents_reclaim`
- `flock_acquire_and_release`
- `claim_next_all_race_lost_returns_all_race_lost`
- `workspace_lock_path_differs_for_different_workspaces`
- `workspace_lock_path_is_deterministic`
- `max_retries_caps_attempts`

### Strand Knot Tests (4 tests)
- `all_claimed_returns_no_work_no_alert`
- `claimed_by_deduplicates_workers`
- `diagnose_all_claimed`
- `diagnose_mixed_done_and_in_progress_is_all_claimed`

### Telemetry Tests (3 tests)
- `test_beads_claimed_increments_with_attributes`
- `test_claim_attempts_tracks_all_results`
- `test_severity_for_bead_claim_race_lost_is_info`

### Worker State Machine Tests (8 tests)
- `do_claim_no_current_bead_resets_to_selecting`
- `do_claim_not_claimable_increments_consecutive_counter`
- `do_claim_not_claimable_transitions_to_retrying`
- `do_claim_race_lost_adds_to_exclusion_and_retries`
- `do_claim_race_lost_increments_consecutive_counter`
- `do_claim_success_resets_consecutive_counter`
- `do_select_with_beads_transitions_to_claiming`

## Result

✅ **All 26 tests passed**  
❌ **0 failed tests**  
⏱️  **Test duration:** 43.12 seconds

The claim module demonstrates robust unit test coverage across core functionality, strand coordination, telemetry integration, and worker state machine transitions.
