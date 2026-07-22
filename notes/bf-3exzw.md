# bf-3exzw: Claim Error Distinction Implementation Summary

## Status: Already Implemented ✓

This feature (P5.5 claim: distinguish claim-error from race-lost) has already been fully implemented in the codebase.

## Implementation Details

### 1. Distinct Error Outcomes (ClaimResult enum)

**File:** `src/types/mod.rs` (lines 335-369)

```rust
pub enum ClaimResult {
    Claimed(Bead),
    RaceLost { claimed_by: String },
    NotClaimable { reason: String },
    ClaimError { reason: String },  // ← NEW: distinct from race-lost
    Suspect {                       // ← NEW: N-threshold escalation
        bead_id: BeadId,
        consecutive_errors: u32,
        last_error: String,
    },
}
```

### 2. Claim Error Tracking (Claimer)

**File:** `src/claim/mod.rs`

- **Threshold constant** (line 30): `CLAIM_ERROR_THRESHOLD: u32 = 3`
- **Error counter** (line 40): `claim_errors: HashMap<BeadId, u32>`
- **Record errors** (lines 68-88): `record_claim_error()` - increments counter, returns Some when threshold reached
- **Clear on success** (lines 90-94): `clear_claim_errors()` - resets counter after successful claim
- **Telemetry** (line 262): Emits `ClaimErrorThreshold` event

### 3. Worker Handling

**File:** `src/worker/mod.rs` (lines 1460-1507)

- Handles `ClaimResult::ClaimError { reason }` - logs, excludes bead, continues to SELECTING
- Handles `ClaimResult::Suspect { ... }` - emits telemetry, marks bead suspect, excludes bead, continues to SELECTING

### 4. Telemetry Events

**File:** `src/telemetry/mod.rs` (lines 198-202)

```rust
ClaimErrorThreshold {
    bead_id: BeadId,
    consecutive_errors: u32,
    last_error: String,
},
```

## Unit Tests (All Passing ✓)

**File:** `src/claim/mod.rs` (lines 867-1282)

1. ✅ `claim_error_returns_error_not_race_lost` - Verifies ClaimError is distinct from RaceLost
2. ✅ `consecutive_claim_errors_trigger_suspect_outcome` - Tests N=3 threshold escalation
3. ✅ `successful_claim_clears_error_counter` - Verifies counter reset on success
4. ✅ `suspect_outcome_includes_consecutive_count` - Tests metadata preservation
5. ✅ `claim_one_preserves_suspect_outcome` - Tests claim_one wrapper behavior

## Code Quality Checks

✅ **Clippy**: `cargo clippy --lib -- -D warnings` - Clean
✅ **Format**: `cargo fmt --check` - Properly formatted
✅ **Exhaustive matches**: All ClaimResult/ClaimOutcome variants explicitly handled
✅ **No unwrap/expect**: All code uses `?` with anyhow

## Acceptance Criteria Met

- ✅ CLI claim failures are distinguished from race-lost (ClaimError vs RaceLost)
- ✅ After N consecutive claim errors (N=3), ERROR telemetry event emitted
- ✅ Bead marked suspect and skipped with reason
- ✅ Outcome handling is exhaustive (no catch-all `_`)
- ✅ Unit tests for both paths (error and race-lost)
- ✅ Unit tests for N-threshold escalation

## Files Modified (Already Committed)

1. `src/types/mod.rs` - Added ClaimError and Suspect variants
2. `src/claim/mod.rs` - Implemented error tracking and threshold logic
3. `src/worker/mod.rs` - Added handling for ClaimError and Suspect outcomes
4. `src/telemetry/mod.rs` - Added ClaimErrorThreshold event kind

## Implementation Notes

- **Threshold**: Fixed at 3 consecutive errors (can be made configurable if needed)
- **Error tracking**: Per-bead counter stored in HashMap within Claimer
- **Reset on success**: Counter cleared immediately after successful claim
- **Telemetry**: ClaimErrorThreshold event includes bead_id, consecutive_errors, last_error
- **Worker behavior**: Suspect beads are excluded and worker returns to SELECTING state

This implementation fully addresses ADR-001 item 5: "Claim-error taxonomy — distinguish error from race; escalate repeated errors via telemetry instead of silent cycling."
