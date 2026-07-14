# Bead bf-11yyg: pluck.starvation_detected Telemetry Event

## Status: Already Implemented

The `PluckStarvationDetected` telemetry event was already fully implemented in the codebase.

## Implementation Location

- **Event Definition**: `src/telemetry/mod.rs` lines 151-156
- **Event Type**: `"strand.pluck.starvation_detected"`
- **Fields**:
  - `workspace: String` - path to the scanned workspace
  - `open_count: usize` - number of open beads found
  - `excluded_count: usize` - number of beads excluded from dispatch
  - `candidate_exclusion_reasons: Vec<String>` - reasons why candidates were excluded

## Usage

The event is already being emitted in `src/strand/knot.rs` when starvation is detected:
```rust
telemetry.emit(crate::telemetry::EventKind::PluckStarvationDetected {
    workspace: workspace_path,
    open_count: *open_count,
    excluded_count,
    candidate_exclusion_reasons,
})
```

## Acceptance Criteria

All acceptance criteria met:
- ✅ Event type is defined in telemetry module
- ✅ Event struct has all required fields
- ✅ Event can be emitted through the telemetry pipeline (actively emitted)
- ✅ No functional code changes needed — just the event type definition (already present)

## Note

The task specification listed the field as `workspace_path` but the implemented code uses `workspace`. These are semantically equivalent - both represent the path to the scanned workspace. The emission code passes the local variable `workspace_path` into the `workspace` field.
